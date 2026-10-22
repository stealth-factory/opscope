// opscope - small dependency-free terminal widgets
// Copyright (C) 2026 William Li
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published
// by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! Google Analytics 4: is traffic normal right now, and did the last
//! change hurt key events?
//!
//! Every figure is GA's - realtime, processed reports, or the property's
//! own quota - or a reading this widget took itself and wrote to its
//! sample file, which is where the realtime baseline comes from: GA keeps
//! no realtime history, and its processed reports run hours behind.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use opscope_core as tc;
use opscope_core::google::{self, Credential};

mod api;
mod draw;
mod model;
mod parse;
mod poll;

use draw::{Item, View};
use model::Sort;
use poll::{Cfg, Prop, Shared};

const SETTINGS: tc::SettingsSpec = tc::SettingsSpec {
    widget: "google-analytics",
    section: "google_analytics",
    legacy_section: None,
    schema: include_str!("settings.json"),
    catalogues: &[],
};

/// The variable a pasted access token is read from by default.
const TOKEN_ENV: &str = "GOOGLE_OAUTH_ACCESS_TOKEN";

/// Property ids from config: numbers or strings, with or without the
/// `properties/` prefix. Anything else is named rather than dropped.
fn property_ids(raw: &[serde_json::Value]) -> (Vec<String>, Vec<String>) {
    let mut ids = Vec::new();
    let mut bad = Vec::new();
    for v in raw {
        let s = match v {
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::String(s) => s.trim().trim_start_matches("properties/").to_string(),
            other => other.to_string(),
        };
        if !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()) {
            if !ids.contains(&s) {
                ids.push(s);
            }
        } else {
            bad.push(s);
        }
    }
    (ids, bad)
}

fn new_prop(id: &str, name: &str, interval: u64) -> Prop {
    Prop { id: id.to_string(), name: name.to_string(), interval, ..Prop::default() }
}

/// The key a detail is held under.
fn detail_key(prop: &str, item: &Item) -> Option<String> {
    match item {
        Item::Page(path) => Some(format!("page:{}:{}", prop, path)),
        Item::Organic(path) => Some(format!("page:{}:{}", prop, landing_path(path))),
        Item::Event(name) => Some(format!("event:{}:{}", prop, name)),
        _ => None,
    }
}

/// A landing page without its query string, which is how `pagePath` -
/// the dimension a page's detail is filtered on - spells it.
fn landing_path(landing: &str) -> String {
    landing.split('?').next().unwrap_or(landing).to_string()
}

fn main() {
    tc::maybe_widget_help(include_str!("help.txt"), include_str!("CONFIGURE.md"), true);
    if !tc::dependencies_available("google-analytics", include_str!("dependencies.json"), Some(SETTINGS)) {
        return;
    }
    tc::record_panics();
    let cfg = tc::load_config("google_analytics");
    let credentials_file = tc::cfg_str(&cfg, "credentials_file", "");
    let token = tc::cfg_str(&cfg, "token", "");
    let token_env = tc::cfg_str(&cfg, "token_env", TOKEN_ENV);
    let raw_properties = cfg.get("properties").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let (ids, bad_ids) = property_ids(&raw_properties);
    let chosen = model::realtime_choice(tc::cfg_f64(&cfg, "realtime_refresh", 15.0));
    let refresh = tc::poll_secs(tc::cfg_f64(&cfg, "refresh", 300.0), 300.0).max(60.0);
    let window = tc::cfg_usize(&cfg, "window_days", 7).clamp(1, 90);
    let weeks = tc::cfg_usize(&cfg, "baseline_weeks", 4).clamp(1, 12);
    let configured: Vec<String> = tc::cfg_strings(&cfg, "key_events", &[])
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let poll_cfg = Cfg { refresh, baseline_weeks: weeks, key_events: configured.clone() };

    let cred = google::credential(&credentials_file, &token, &token_env);
    let email = cred.as_ref().and_then(Credential::client_email);
    let grant = api::grant(email.as_deref());
    let activity = tc::Refresh::default();
    let shared = Arc::new(Mutex::new(Shared { chosen, window, ..Shared::default() }));
    if let Ok(mut g) = shared.lock() {
        if !bad_ids.is_empty() {
            g.discovery_err = format!(
                "skipped properties that are not numeric GA4 property ids: {}",
                bad_ids.join(", ")
            );
        }
    }

    match &cred {
        None => {
            if let Ok(mut g) = shared.lock() {
                g.discovery_err = tc::missing_config(&format!(
                    "no credential: set google_analytics.credentials_file to a service-account key \
                     (docs/google-setup.md), or paste a token in google_analytics.token or ${}",
                    if token_env.is_empty() { TOKEN_ENV } else { token_env.as_str() }
                ));
            }
        }
        Some(cred) if !ids.is_empty() => {
            if let Ok(mut g) = shared.lock() {
                g.props = ids.iter().map(|id| new_prop(id, "", chosen)).collect();
            }
            for i in 0..ids.len() {
                poll::spawn_property(
                    Arc::clone(&shared),
                    i,
                    cred.clone(),
                    grant.clone(),
                    poll_cfg.clone(),
                    activity.clone(),
                );
            }
        }
        Some(cred) => {
            let (shared, cred, grant, poll_cfg, activity) =
                (Arc::clone(&shared), cred.clone(), grant.clone(), poll_cfg.clone(), activity.clone());
            let who = email.clone().unwrap_or_else(|| "this token's account".into());
            std::thread::spawn(move || {
                let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| loop {
                    if let Ok(mut g) = shared.lock() {
                        g.discovery = "listing the GA4 properties this account can see...".into();
                    }
                    let work = activity.start();
                    let found = poll::discover(&cred, &grant);
                    drop(work);
                    let wait = match found {
                        Ok(list) if list.is_empty() => {
                            if let Ok(mut g) = shared.lock() {
                                g.discovery.clear();
                                g.discovery_err = format!(
                                    "no GA4 properties visible to {} - add it as Viewer on a property \
                                     (Property access management), or list property ids in properties",
                                    who
                                );
                            }
                            300
                        }
                        Ok(list) => {
                            let n = list.len();
                            if let Ok(mut g) = shared.lock() {
                                let chosen = g.chosen;
                                g.props = list.iter().map(|p| new_prop(&p.id, &p.name, chosen)).collect();
                                g.discovery.clear();
                                g.discovery_err.clear();
                            }
                            for i in 0..n {
                                poll::spawn_property(
                                    Arc::clone(&shared),
                                    i,
                                    cred.clone(),
                                    grant.clone(),
                                    poll_cfg.clone(),
                                    activity.clone(),
                                );
                            }
                            return;
                        }
                        Err(f) => {
                            if let Ok(mut g) = shared.lock() {
                                g.discovery.clear();
                                g.discovery_err = format!("listing properties: {}", f.text);
                            }
                            60
                        }
                    };
                    std::thread::sleep(Duration::from_secs(wait));
                }));
                if let Err(payload) = run {
                    if let Ok(mut g) = shared.lock() {
                        g.discovery.clear();
                        g.discovery_err = format!("property discovery stopped: {}", poll::panic_reason(payload.as_ref()));
                    }
                }
            });
        }
    }

    tc::setup();
    let mut keyboard = tc::Keyboard::new();
    let p = draw::palette();
    let mut view = View {
        prop: 0,
        metric: 0,
        sort: Sort::Change,
        needle: String::new(),
        typing: false,
        selected: 0,
        configured: configured.clone(),
        weeks,
        cred_note: cred.as_ref().and_then(Credential::warning),
        now: tc::now(),
    };
    let mut scroll = 0usize;
    let mut moved = false;
    let mut visible = 1usize;
    let (mut placed, mut head): (Vec<(usize, usize)>, usize) = (Vec::new(), 1);
    let mut items: Vec<Item> = Vec::new();
    // The open detail, if any, and how far down it is scrolled.
    let mut detail: Option<Item> = None;
    let mut dscroll = 0usize;
    let mut note: (String, f64) = (String::new(), 0.0);
    let mut frames = 0usize;
    // Details already asked for, so a frame does not ask again while one
    // is on its way.
    let mut asked: std::collections::HashSet<String> = std::collections::HashSet::new();

    loop {
        let tick = tc::animation_tick();
        frames += 1;
        if frames % 40 == 0 {
            // The key file's mode can change under a running pane.
            view.cred_note = cred.as_ref().and_then(Credential::warning);
        }
        let mut keys = keyboard.poll();
        if detail.is_none() {
            if let Some(at) = tc::rows_clicked(&mut keys, Some(view.selected), head, scroll, &placed, None) {
                view.selected = at;
                moved = true;
            }
        }
        let n_props = shared.lock().map(|g| g.props.len()).unwrap_or(0);
        let prop_id = shared
            .lock()
            .ok()
            .and_then(|g| g.props.get(view.prop).map(|p| p.id.clone()))
            .unwrap_or_default();
        for key in keys {
            if view.typing && detail.is_none() {
                match key.as_str() {
                    "esc" => {
                        view.needle.clear();
                        view.typing = false;
                    }
                    "enter" => view.typing = false,
                    "backspace" => {
                        view.needle.pop();
                    }
                    other if other.chars().count() == 1 => view.needle.push_str(other),
                    _ => {}
                }
                view.selected = 0;
                moved = true;
                continue;
            }
            match key.as_str() {
                "q" | "Q" => {
                    keyboard.restore();
                    tc::restore_screen();
                    return;
                }
                "," => {
                    tc::run_settings(&mut keyboard, SETTINGS);
                    continue;
                }
                "r" | "R" => {
                    if let Ok(mut g) = shared.lock() {
                        g.force += 1;
                        if let Some(open) = detail.as_ref().and_then(|d| detail_key(&prop_id, d)) {
                            g.details.remove(&open);
                            asked.remove(&open);
                        }
                    }
                }
                "c" | "C" => {
                    if !prop_id.is_empty() {
                        let url = api::ga_url(&prop_id);
                        note = (
                            if tc::clipboard(&url) {
                                format!("copied {}", url)
                            } else {
                                format!("no clipboard here - the URL is {}", url)
                            },
                            tc::now() + 4.0,
                        );
                    }
                }
                "esc" if detail.is_some() => detail = None,
                "ctrl-y" | "wheel-up" => {
                    if detail.is_some() {
                        dscroll = dscroll.saturating_sub(1);
                    } else {
                        scroll = scroll.saturating_sub(1);
                    }
                }
                "ctrl-e" | "wheel-down" => {
                    if detail.is_some() {
                        dscroll = dscroll.saturating_add(1);
                    } else {
                        scroll = scroll.saturating_add(1);
                    }
                }
                "up" | "k" if detail.is_some() => dscroll = dscroll.saturating_sub(1),
                "down" | "j" if detail.is_some() => dscroll = dscroll.saturating_add(1),
                "pgup" if detail.is_some() => dscroll = dscroll.saturating_sub(visible.max(1)),
                "pgdn" if detail.is_some() => dscroll = dscroll.saturating_add(visible.max(1)),
                "home" if detail.is_some() => dscroll = 0,
                "end" if detail.is_some() => dscroll = usize::MAX / 2,
                _ if detail.is_some() => {}
                "up" | "k" => {
                    view.selected = view.selected.saturating_sub(1);
                    moved = true;
                }
                "down" | "j" => {
                    view.selected += 1;
                    moved = true;
                }
                "pgup" => {
                    view.selected = view.selected.saturating_sub(visible.max(1));
                    moved = true;
                }
                "pgdn" => {
                    view.selected += visible.max(1);
                    moved = true;
                }
                "home" => {
                    view.selected = 0;
                    moved = true;
                }
                "end" => {
                    view.selected = items.len().saturating_sub(1);
                    moved = true;
                }
                "tab" => {
                    let here = items.get(view.selected).map(draw::section_of).unwrap_or(0);
                    let next = items
                        .iter()
                        .position(|i| draw::section_of(i) > here)
                        .unwrap_or(0);
                    view.selected = next;
                    moved = true;
                }
                "left" | "right" if n_props > 1 => {
                    view.prop = if key == "left" {
                        (view.prop + n_props - 1) % n_props
                    } else {
                        (view.prop + 1) % n_props
                    };
                    view.selected = 0;
                    scroll = 0;
                }
                "enter" => match items.get(view.selected) {
                    Some(Item::Property(i)) => {
                        view.prop = *i;
                        detail = Some(Item::Property(*i));
                        dscroll = 0;
                    }
                    Some(item @ (Item::Page(_) | Item::Organic(_) | Item::Event(_))) => {
                        detail = Some(item.clone());
                        dscroll = 0;
                    }
                    Some(_) => {
                        note = ("channels and sources have no detail; ↵ opens pages, key events and properties".into(), tc::now() + 4.0)
                    }
                    None => {}
                },
                "m" | "M" => view.metric = (view.metric + 1) % draw::METRICS.len(),
                "s" | "S" => view.sort = view.sort.next(),
                "/" => view.typing = true,
                "w" | "W" => {
                    if let Ok(mut g) = shared.lock() {
                        g.window = tc::cycle(model::WINDOW_CHOICES, g.window);
                    }
                }
                "l" | "L" => {
                    if let Ok(mut g) = shared.lock() {
                        g.chosen = tc::cycle(model::LIVE_CHOICES, g.chosen);
                        g.force += 1;
                    }
                }
                _ => {}
            }
        }

        let (w, h) = tc::size();
        view.now = tc::now();
        if !note.0.is_empty() && view.now > note.1 {
            note = (String::new(), 0.0);
        }
        let title = tc::title_busy("google analytics", w, &p.head, activity.tick(tick));
        let guard = match shared.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        let (window_now, chosen_now) = (guard.window, guard.chosen);
        if view.prop >= guard.props.len() {
            view.prop = 0;
        }

        if let Some(open) = detail.clone() {
            // A detail has no rows to pick, and the list's placements
            // describe a frame no longer on screen.
            placed.clear();
            let prop = guard.props.get(view.prop).cloned().unwrap_or_default();
            let key = detail_key(&prop.id, &open);
            let held = key.as_ref().and_then(|k| guard.details.get(k).cloned());
            drop(guard);
            // A detail older than five minutes is asked for again, and one
            // already asked for is not asked twice while it is on its way.
            if let (Some(k), Some(d)) = (key.as_ref(), held.as_ref()) {
                if view.now - d.at > 300.0 {
                    if let Ok(mut g) = shared.lock() {
                        g.details.remove(k);
                    }
                    asked.remove(k);
                }
            }
            if let Some(k) = key.as_ref().filter(|k| !asked.contains(*k)) {
                let body = match &open {
                    Item::Page(path) => Some(api::page_detail_body(path)),
                    Item::Organic(path) => Some(api::page_detail_body(&landing_path(path))),
                    Item::Event(name) => {
                        Some(api::event_detail_body(name, if configured.is_empty() { "keyEvents" } else { "eventCount" }))
                    }
                    _ => None,
                };
                if let (Some(body), Some(cred)) = (body, cred.clone()) {
                    asked.insert(k.clone());
                    poll::spawn_detail(Arc::clone(&shared), k.clone(), cred, grant.clone(), prop.id.clone(), body, activity.clone());
                }
            }
            let held = key.as_ref().and_then(|k| shared.lock().ok().and_then(|g| g.details.get(k).cloned()));
            let reports = held.as_ref().map(|d| d.reports.clone());
            let lines = match &open {
                Item::Property(_) => draw::property_detail(&prop, &view, w),
                Item::Page(path) => draw::page_detail(path, reports.as_ref(), w),
                Item::Organic(path) => {
                    let mut l = draw::page_detail(&landing_path(path), reports.as_ref(), w);
                    l.insert(1, vec![(p.hint.clone(), format!(" landing page {} as Google Search visitors arrived on it", path))]);
                    l
                }
                Item::Event(name) => draw::event_detail(
                    name,
                    reports.as_ref(),
                    if configured.is_empty() { "keyEvents" } else { "eventCount" },
                    w,
                ),
                _ => Vec::new(),
            };
            let mut hints: Vec<Vec<(&str, String)>> = vec![
                vec![(p.hint.as_str(), "esc back".into())],
                vec![(p.hint.as_str(), "[r]efresh".into())],
                vec![(p.hint.as_str(), "[c]opy URL".into())],
                vec![(p.hint.as_str(), "[,] settings".into())],
                vec![(p.hint.as_str(), "[q]uit".into())],
            ];
            if !note.0.is_empty() {
                hints.insert(0, vec![(p.good.as_str(), note.0.clone())]);
            }
            let packed = tc::pack_hints_placed(&hints, w.saturating_sub(2), "  ");
            let foot: Vec<String> = packed.lines.iter().map(|l| format!(" {}", l)).collect();
            let room = h.saturating_sub(foot.len() + 1).max(1);
            visible = room;
            let furthest = lines.len().saturating_sub(room);
            dscroll = dscroll.min(furthest);
            let mut rows = vec![title];
            for line in lines.iter().skip(dscroll).take(room) {
                let refs: Vec<(&str, String)> = line.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
                rows.push(tc::seg(&refs, w.saturating_sub(1)));
            }
            while rows.len() < room + 1 {
                rows.push(String::new());
            }
            let foot_top = rows.len();
            rows.extend(foot);
            tc::draw(&rows, w, h);
            keyboard.footer_at(&packed, foot_top, 1);
            keyboard.wait(Duration::from_millis(200));
            continue;
        }

        let frame = draw::body(&guard, &view, w);
        let multi = guard.props.len() > 1;
        drop(guard);
        items = frame.items.clone();
        if !items.is_empty() && view.selected >= items.len() {
            view.selected = items.len() - 1;
        }
        let mut rows = vec![title];
        for line in &frame.lines {
            let refs: Vec<(&str, String)> = line.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
            rows.push(tc::seg(&refs, w.saturating_sub(1)));
        }
        let next_metric = draw::METRICS[(view.metric + 1) % draw::METRICS.len()].1;
        let mut hints: Vec<Vec<(&str, String)>> = Vec::new();
        if !note.0.is_empty() {
            hints.push(vec![(p.good.as_str(), note.0.clone())]);
        }
        hints.push(vec![(p.hint.as_str(), "↑↓ row".into())]);
        hints.push(vec![(p.hint.as_str(), "tab section".into())]);
        if multi {
            hints.push(vec![(p.hint.as_str(), "←→ property".into())]);
        }
        hints.extend([
            vec![(p.hint.as_str(), "↵ detail".to_string())],
            vec![(p.hint.as_str(), format!("[m]etric {}", next_metric))],
            vec![(p.hint.as_str(), format!("[w]indow {}d", tc::cycle(model::WINDOW_CHOICES, window_now)))],
            vec![(p.hint.as_str(), format!("[s]ort {}", view.sort.next().word()))],
            vec![(p.hint.as_str(), "[/]filter".to_string())],
            vec![(p.hint.as_str(), format!("[l]ive {}s", tc::cycle(model::LIVE_CHOICES, chosen_now)))],
            vec![(p.hint.as_str(), "[c]opy URL".to_string())],
            vec![(p.hint.as_str(), "[r]efresh".to_string())],
            vec![(p.hint.as_str(), "[,] settings".to_string())],
            vec![(p.hint.as_str(), "[q]uit".to_string())],
        ]);
        let packed = tc::pack_hints_placed(&hints, w.saturating_sub(2), "  ");
        let foot: Vec<String> = packed.lines.iter().map(|l| format!(" {}", l)).collect();
        let room = h.saturating_sub(foot.len()).max(2);
        let below = room - 1;
        visible = frame.placed.iter().map(|(_, i)| *i).collect::<std::collections::BTreeSet<_>>().len().min(below.max(1));
        let body_len = rows.len() - 1;
        if moved {
            if let Some(at) = frame.cursor {
                scroll = tc::follow(scroll, at, below);
            }
            moved = false;
        }
        scroll = scroll.min(body_len.saturating_sub(below));
        head = 1;
        placed = frame.placed.iter().map(|(row, i)| (row + 1, *i)).collect();
        let mut out = vec![rows[0].clone()];
        out.extend(rows.iter().skip(1 + scroll).take(below).cloned());
        while out.len() < room {
            out.push(String::new());
        }
        let foot_top = out.len();
        out.extend(foot);
        tc::draw(&out, w, h);
        keyboard.footer_at(&packed, foot_top, 1);
        keyboard.wait(Duration::from_millis(250));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn property_ids_take_numbers_and_name_the_rest() {
        let raw: Vec<serde_json::Value> =
            serde_json::from_str(r#"[123, "456", "properties/789", "a789p123", "", "456"]"#).unwrap();
        let (ids, bad) = property_ids(&raw);
        assert_eq!(ids, vec!["123", "456", "789"]);
        assert_eq!(bad, vec!["a789p123", ""]);
    }

    #[test]
    fn a_landing_page_opens_as_its_path() {
        assert_eq!(landing_path("/blog/launch?utm_source=x"), "/blog/launch");
        assert_eq!(landing_path("/pricing"), "/pricing");
        assert_eq!(detail_key("123", &Item::Organic("/a?b=c".into())), Some("page:123:/a".into()));
        assert_eq!(detail_key("123", &Item::Channel("Direct".into())), None);
    }

    #[test]
    fn the_token_variable_is_the_one_google_tooling_uses() {
        assert_eq!(TOKEN_ENV, google::DEFAULT_TOKEN_ENV);
    }
}
