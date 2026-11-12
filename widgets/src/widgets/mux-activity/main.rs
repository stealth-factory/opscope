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

//! Every pane of the multiplexer this runs in, ordered by who needs you.
//!
//! `mux-activity` finds out whether it is inside Rex, Herdr or Luvus from the
//! markers each puts in its panes' environment, and draws that
//! multiplexer's board: `rex.rs`, `herdr.rs` and `luvus.rs`, each with its
//! own reader and event stream, and only the detected one runs. Outside all
//! three it says so and exits before the terminal is taken over.

use opscope_core as tc;

#[path = "../../process_filter.rs"]
mod process_filter;

#[path = "../refresh_wake.rs"]
mod refresh_wake;

#[path = "detect.rs"]
mod detect;

#[path = "background.rs"]
mod background;

#[path = "rex.rs"]
mod rex;

#[path = "rex_parse.rs"]
mod rex_parse;

#[path = "rex_events.rs"]
mod rex_events;

#[path = "herdr.rs"]
mod herdr;

#[path = "herdr_parse.rs"]
mod herdr_parse;

#[path = "herdr_events.rs"]
mod herdr_events;

#[path = "luvus.rs"]
mod luvus;

#[path = "luvus_parse.rs"]
mod luvus_parse;

#[path = "luvus_events.rs"]
mod luvus_events;

#[cfg(target_os = "linux")]
#[path = "linux.rs"]
mod host;
#[cfg(target_os = "macos")]
#[path = "macos.rs"]
mod host;

use detect::{Ask, Markers, Mux};

const SETTINGS: tc::SettingsSpec = tc::SettingsSpec {
    widget: "mux-activity",
    section: "mux_activity",
    legacy_section: None,
    schema: include_str!("settings.json"),
    catalogues: &[],
};

fn main() {
    tc::maybe_widget_help(include_str!("help.txt"), include_str!("CONFIGURE.md"), true);
    tc::register_settings(SETTINGS);
    let args: Vec<String> = std::env::args().skip(1).collect();
    let file = first_config().unwrap_or_else(|| serde_json::json!({}));
    let has_section = file.get("mux_activity").is_some();
    // What decides the backend is read before there is one: the panes
    // section, or the rex_panes one that held `server` before the rename.
    let deciding = if has_section { tc::load_config("mux_activity") } else { old_section(Mux::Rex, &file) };
    let flag = args
        .iter()
        .position(|a| a == "--mux")
        .map(|at| args.get(at + 1).map(String::as_str).unwrap_or(""));
    let setting = tc::cfg_str(&deciding, "backend", "auto");
    let rex_server = tc::cfg_str(&deciding, "rex_server", "");
    let markers = Markers::read(|name| std::env::var(name).ok());
    // The tree is only walked when it has something to settle.
    let several = [markers.rex, markers.herdr, markers.luvus].iter().filter(|m| **m).count() > 1;
    let ancestors = if several { ancestors() } else { Vec::new() };
    let ask = Ask {
        flag,
        setting: &setting,
        rex_server: &rex_server,
        markers,
        ancestors: &ancestors,
    };
    let chosen = match detect::choose(&ask) {
        Ok(chosen) => chosen,
        // Plain text on stderr and a failing status, before the terminal is
        // taken: a script that started `mux-activity` in the wrong place hears why.
        Err(why) => {
            eprintln!("{}", why);
            std::process::exit(2);
        }
    };
    let cfg = if has_section { deciding } else { old_section(chosen.mux, &file) };
    let needs = dependencies_for(chosen.mux, include_str!("dependencies.json"));
    if !tc::dependencies_available("mux-activity", &needs, Some(SETTINGS)) {
        return;
    }
    // `,` opens one screen for every backend: they share one section.
    let comma = |keyboard: &mut tc::Keyboard| tc::run_settings(keyboard, SETTINGS);
    let line = format!("in {} · {}", chosen.mux.name(), chosen.why);
    match chosen.mux {
        Mux::Rex => rex::watch(&cfg, &args, &line, &comma),
        Mux::Herdr => herdr::watch(&cfg, &args, &line, &comma),
        Mux::Luvus => luvus::watch(&cfg, &args, &line, &comma),
    }
}

/// The pinned line under every board's title: which multiplexer this is
/// reading and what decided it, `in Rex · REX_SESSION and REX_BLOCK are
/// set`. Wrapped rather than cut, because the reason is the part that
/// answers "why this one" when several were set.
pub fn mux_rows(line: &str, colour: &str, w: usize) -> Vec<String> {
    tc::wrap_words(line, w.saturating_sub(2).max(1))
        .into_iter()
        .map(|l| tc::seg(&[(colour, format!(" {}", l))], w.saturating_sub(1)))
        .collect()
}

/// A group's sub-heading - a Rex session, a Herdr workspace - with a rule
/// to the pane's edge: its name, how many panes it holds, then the line
/// that says one group has ended and the next begun. The rule is a hairline
/// in Kitty (`Plot::rule`) and `─` in text, written into the row as well so
/// the row reads the same without the plot. `y` is the row the heading
/// lands on in whatever the plots are placed against.
///
/// A name too long for the pane wraps whole, without a rule, rather than
/// being cut: a cut name reads as a different group.
pub fn group_heading(
    label: &str,
    n: usize,
    w: usize,
    (name, count, rule): (&str, &str, &str),
    y: usize,
    plots: &mut Vec<tc::Plot>,
) -> Vec<String> {
    let tally = format!("· {}", n);
    let room = w.saturating_sub(1);
    let used = 1 + tc::display_width(label) + 1 + tc::display_width(&tally);
    if used + 3 < room {
        let x = used + 1;
        let width = room - x;
        plots.push(tc::Plot::rule(x, y, width, rule, "─", 0.08));
        return vec![tc::seg(
            &[
                ("", " ".to_string()),
                (name, label.to_string()),
                ("", " ".to_string()),
                (count, tally),
                ("", " ".to_string()),
                (rule, "─".repeat(width)),
            ],
            room,
        )];
    }
    tc::wrap_words(&format!("{} {}", label, tally), w.saturating_sub(2).max(1))
        .into_iter()
        .map(|l| tc::seg(&[(name, format!(" {}", l))], room))
        .collect()
}

/// What the dependency check asks for: the one multiplexer CLI this
/// backend talks to, made required, and what it alone recommends.
///
/// `dependencies.json` lists all three as recommended, so `opscope doctor`
/// does not call `mux-activity` broken on a machine with only one of them; a pane
/// inside Herdr with no `herdr` on PATH still gets the shared "cannot start ·
/// needs herdr" screen.
fn dependencies_for(mux: Mux, all: &str) -> String {
    let all: serde_json::Value = serde_json::from_str(all).unwrap_or_else(|_| serde_json::json!({}));
    let mut required = serde_json::Map::new();
    let mut recommended = serde_json::Map::new();
    if let Some(entries) = all.get("recommended").and_then(|r| r.as_object()) {
        for (tool, entry) in entries {
            if tool == mux.key() {
                required.insert(tool.clone(), entry.clone());
            } else if Mux::ALL.iter().all(|m| m.key() != tool) && only_for(tool) == Some(mux) {
                recommended.insert(tool.clone(), entry.clone());
            }
        }
    }
    serde_json::json!({ "required": required, "recommended": recommended }).to_string()
}

/// A recommended tool that only one backend uses.
fn only_for(tool: &str) -> Option<Mux> {
    match tool {
        // luvus-panes matches opscope's own panes by local process name.
        "ps" => Some(Mux::Luvus),
        _ => None,
    }
}

/// A section left from before `mux-activity`: rex_panes, herdr_panes or
/// luvus_panes, read when there is no `mux_activity` section. Keys that were
/// renamed are carried over - `server` is now `rex_server`, `session`
/// `luvus_session` - and a notice asks for the rename. `load_config` returns
/// `{}` for a missing section, so presence is read off the file itself, as
/// agent-usage does it.
fn old_section(mux: Mux, file: &serde_json::Value) -> serde_json::Value {
    let (name, mut old) = match mux {
        Mux::Rex => ("rex_panes", tc::load_config("rex_panes")),
        Mux::Herdr => ("herdr_panes", tc::load_config("herdr_panes")),
        Mux::Luvus => ("luvus_panes", tc::load_config("luvus_panes")),
    };
    if file.get(name).is_none() {
        return serde_json::json!({});
    }
    if let Some(map) = old.as_object_mut() {
        for (was, now) in [("server", "rex_server"), ("session", "luvus_session")] {
            if let Some(value) = map.remove(was) {
                map.insert(now.into(), value);
            }
        }
    }
    tc::notices::when(true, || {
        tc::notices::Notice::info(
            "mux-activity-old-section",
            "config: an old rex_panes, herdr_panes or luvus_panes section is read because there is no mux_activity section - rename it to mux_activity, server to rex_server and session to luvus_session",
        )
    });
    old
}

fn first_config() -> Option<serde_json::Value> {
    tc::config_paths()
        .into_iter()
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .find_map(|text| serde_json::from_str(&text).ok())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn ancestors() -> Vec<String> {
    host::ancestors()
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn ancestors() -> Vec<String> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The text of a row, its colour escapes dropped.
    fn strip(row: &str) -> String {
        let mut out = String::new();
        let mut chars = row.chars();
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

    #[test]
    fn the_multiplexer_line_wraps_whole_at_any_width() {
        let line = "in Herdr · REX_SESSION and REX_BLOCK, HERDR_ENV=1 are all set; Herdr is the nearest multiplexer above this process";
        for w in [24, 40, 80, 140] {
            let rows = mux_rows(line, "", w);
            let plain: Vec<String> = rows.iter().map(|r| strip(r)).collect();
            for r in &plain {
                assert!(tc::display_width(r) < w, "{w}: {r:?}");
            }
            let joined = plain.iter().map(|r| r.trim()).collect::<Vec<_>>().join(" ");
            assert_eq!(joined, line, "{w}: nothing dropped");
        }
        assert_eq!(mux_rows("in Rex · REX_SESSION and REX_BLOCK are set", "", 80).len(), 1);
    }

    #[test]
    fn a_group_heading_too_wide_for_its_rule_wraps_whole_without_one() {
        let label = "a-workspace-label-longer-than-the-pane";
        for w in [12, 24, 40] {
            let mut plots = Vec::new();
            let rows = group_heading(label, 3, w, ("", "", ""), 0, &mut plots);
            assert!(plots.is_empty(), "{w}: no rule on a wrapped heading");
            let plain: Vec<String> = rows.iter().map(|r| strip(r)).collect();
            for r in &plain {
                assert!(tc::display_width(r) < w, "{w}: {r:?}");
                assert!(!r.contains('─'), "{w}: {r:?}");
            }
            assert!(plain.concat().contains("· 3"), "{w}: the count survives");
        }
        // Wide enough: one row and one rule.
        let mut plots = Vec::new();
        assert_eq!(group_heading(label, 3, 80, ("", "", ""), 0, &mut plots).len(), 1);
        assert_eq!(plots.len(), 1);
    }

    #[test]
    fn each_backend_requires_only_its_own_cli() {
        let all = include_str!("dependencies.json");
        for mux in Mux::ALL {
            let needs: serde_json::Value = serde_json::from_str(&dependencies_for(mux, all)).unwrap();
            let required: Vec<&String> = needs["required"].as_object().unwrap().keys().collect();
            assert_eq!(required, vec![mux.key()], "{mux:?}");
        }
        let luvus: serde_json::Value = serde_json::from_str(&dependencies_for(Mux::Luvus, all)).unwrap();
        assert!(luvus["recommended"].get("ps").is_some());
        let rex: serde_json::Value = serde_json::from_str(&dependencies_for(Mux::Rex, all)).unwrap();
        assert!(rex["recommended"].get("ps").is_none());
    }
}
