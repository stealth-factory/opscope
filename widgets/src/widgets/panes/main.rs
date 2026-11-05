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
//! multiplexer's board. Outside all three it says so and exits before the
//! terminal is taken over. Rex is read today; Herdr and Luvus still have
//! herdr-panes and luvus-panes until their backends move in here.

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
    legacy_section: Some("rex_panes"),
    schema: include_str!("settings.json"),
    catalogues: &[],
};

fn main() {
    tc::maybe_widget_help(include_str!("help.txt"), include_str!("CONFIGURE.md"), true);
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cfg = config();
    let flag = args
        .iter()
        .position(|a| a == "--mux")
        .map(|at| args.get(at + 1).map(String::as_str).unwrap_or(""));
    let setting = tc::cfg_str(&cfg, "backend", "auto");
    let rex_server = tc::cfg_str(&cfg, "rex_server", "");
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
    match chosen.mux {
        Mux::Rex => {
            if !tc::dependencies_available("panes", include_str!("dependencies.json"), Some(SETTINGS)) {
                return;
            }
            // `,` opens one screen for every backend: they share one section.
            rex::watch(&cfg, &args, &chosen.why, &|keyboard| tc::run_settings(keyboard, SETTINGS));
        }
        other => {
            eprintln!(
                "panes found {} ({}), and does not read {} yet: run {}-panes there for now.",
                other.name(),
                chosen.why,
                other.name(),
                other.key()
            );
            std::process::exit(2);
        }
    }
}

/// The `panes` section, or a `rex_panes` one left from before the rename.
///
/// `load_config` returns `{}` for a missing section, so presence decides,
/// as agent-usage does it. A leftover section is read - its `server`
/// becomes `rex_server` - and a notice says to rename it.
fn config() -> serde_json::Value {
    let present = |name: &str| first_config().is_some_and(|c| c.get(name).is_some());
    if present("panes") || !present("rex_panes") {
        return tc::load_config("panes");
    }
    let mut old = tc::load_config("rex_panes");
    if let Some(map) = old.as_object_mut() {
        if let Some(server) = map.remove("server") {
            map.insert("rex_server".into(), server);
        }
    }
    tc::notices::when(true, || {
        tc::notices::Notice::info(
            "panes-old-section",
            "config: rex_panes is read because there is no panes section - rename it to panes, and its server to rex_server",
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
