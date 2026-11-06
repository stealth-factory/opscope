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
//! `panes` finds out whether it is inside Rex, Herdr or Luvus from the
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
    widget: "panes",
    section: "panes",
    legacy_section: None,
    schema: include_str!("settings.json"),
    catalogues: &[],
};

fn main() {
    tc::maybe_widget_help(include_str!("help.txt"), include_str!("CONFIGURE.md"), true);
    let args: Vec<String> = std::env::args().skip(1).collect();
    let file = first_config().unwrap_or_else(|| serde_json::json!({}));
    let has_panes = file.get("panes").is_some();
    // What decides the backend is read before there is one: the panes
    // section, or the rex_panes one that held `server` before the rename.
    let deciding = if has_panes { tc::load_config("panes") } else { old_section(Mux::Rex, &file) };
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
        // taken: a script that started `panes` in the wrong place hears why.
        Err(why) => {
            eprintln!("{}", why);
            std::process::exit(2);
        }
    };
    let cfg = if has_panes { deciding } else { old_section(chosen.mux, &file) };
    let needs = dependencies_for(chosen.mux, include_str!("dependencies.json"));
    if !tc::dependencies_available("panes", &needs, Some(SETTINGS)) {
        return;
    }
    // `,` opens one screen for every backend: they share one section.
    let comma = |keyboard: &mut tc::Keyboard| tc::run_settings(keyboard, SETTINGS);
    match chosen.mux {
        Mux::Rex => rex::watch(&cfg, &args, &chosen.why, &comma),
        Mux::Herdr => herdr::watch(&cfg, &args, &comma),
        Mux::Luvus => luvus::watch(&cfg, &args, &comma),
    }
}

/// What the dependency check asks for: the one multiplexer CLI this
/// backend talks to, made required, and what it alone recommends.
///
/// `dependencies.json` lists all three as recommended, so `opscope doctor`
/// does not call `panes` broken on a machine with only one of them; a pane
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

/// A section left from before `panes`: rex_panes, herdr_panes or
/// luvus_panes, read when there is no `panes` section. Keys that were
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
            "panes-old-section",
            "config: an old rex_panes, herdr_panes or luvus_panes section is read because there is no panes section - rename it to panes, server to rex_server and session to luvus_session",
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
