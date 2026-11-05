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

//! Which multiplexer this pane is running in.
//!
//! Each multiplexer marks the panes it starts in their environment, and a
//! program started inside a pane inherits every marker above it: Herdr run
//! from a Rex pane leaves `REX_SESSION` set in every Herdr pane. So one
//! marker decides on its own, and several are settled by the process tree -
//! the nearest multiplexer above this process is the one whose pane it is.
//!
//! Pure functions only: what the environment and the tree say goes in, the
//! choice and the reason for it come out. Reading `/proc` or `ps` is the
//! host module's job.

/// The multiplexers `panes` can read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mux {
    Rex,
    Herdr,
    Luvus,
}

impl Mux {
    pub const ALL: [Mux; 3] = [Mux::Rex, Mux::Herdr, Mux::Luvus];

    pub fn name(self) -> &'static str {
        match self {
            Mux::Rex => "Rex",
            Mux::Herdr => "Herdr",
            Mux::Luvus => "Luvus",
        }
    }

    /// The word `--mux` and the `backend` setting take.
    pub fn key(self) -> &'static str {
        match self {
            Mux::Rex => "rex",
            Mux::Herdr => "herdr",
            Mux::Luvus => "luvus",
        }
    }

    /// The marker, as the explain screen names it.
    fn marker(self) -> &'static str {
        match self {
            Mux::Rex => "REX_SESSION and REX_BLOCK",
            Mux::Herdr => "HERDR_ENV=1",
            Mux::Luvus => "LUVUS_ENV=1",
        }
    }
}

/// `rex`, `herdr` or `luvus`, any case. `auto` and empty are not a
/// backend: they mean "find out".
pub fn parse_mux(word: &str) -> Option<Mux> {
    let word = word.trim().to_ascii_lowercase();
    Mux::ALL.into_iter().find(|m| m.key() == word)
}

/// The markers found in this process's environment.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Markers {
    pub rex: bool,
    pub herdr: bool,
    pub luvus: bool,
}

impl Markers {
    /// From a variable lookup, so a test can hand in any environment.
    pub fn read(var: impl Fn(&str) -> Option<String>) -> Markers {
        let set = |name: &str| var(name).is_some_and(|v| !v.is_empty());
        let one = |name: &str| var(name).is_some_and(|v| v == "1");
        Markers {
            rex: set("REX_SESSION") && set("REX_BLOCK"),
            herdr: one("HERDR_ENV"),
            luvus: one("LUVUS_ENV"),
        }
    }

    fn found(self) -> Vec<Mux> {
        Mux::ALL
            .into_iter()
            .filter(|m| match m {
                Mux::Rex => self.rex,
                Mux::Herdr => self.herdr,
                Mux::Luvus => self.luvus,
            })
            .collect()
    }
}

/// The multiplexer a process name belongs to: the server whose child a
/// pane's shell is. `rex`, `herdr` and `luvus`, with or without a path.
pub fn mux_of_process(name: &str) -> Option<Mux> {
    let base = name.rsplit('/').next().unwrap_or(name);
    parse_mux(base)
}

/// What decided the backend, for the explain screen.
#[derive(Clone, Debug, PartialEq)]
pub struct Chosen {
    pub mux: Mux,
    pub why: String,
}

/// What `panes` was told and what it found.
pub struct Ask<'a> {
    /// `--mux`, when given.
    pub flag: Option<&'a str>,
    /// The `backend` setting; empty or `auto` to find out.
    pub setting: &'a str,
    /// A Rex server set in config, which is watched from anywhere.
    pub rex_server: &'a str,
    pub markers: Markers,
    /// Process names from this process's parent upward.
    pub ancestors: &'a [String],
}

/// Pick the backend, or say why there is none.
///
/// A flag beats the setting, which beats everything found. With nothing
/// forced, one marker decides; several are settled by the nearest
/// multiplexer in the process tree; a configured Rex server counts as being
/// inside Rex, because rex-panes has always watched one from anywhere.
pub fn choose(ask: &Ask) -> Result<Chosen, String> {
    if let Some(flag) = ask.flag {
        return match parse_mux(flag) {
            Some(mux) => Ok(Chosen { mux, why: format!("--mux {}", mux.key()) }),
            None => Err(format!(
                "panes: --mux takes rex, herdr or luvus, not {:?}",
                flag
            )),
        };
    }
    let setting = ask.setting.trim();
    if !setting.is_empty() && !setting.eq_ignore_ascii_case("auto") {
        return match parse_mux(setting) {
            Some(mux) => Ok(Chosen { mux, why: format!("backend setting is {}", mux.key()) }),
            None => Err(format!(
                "panes: the backend setting takes auto, rex, herdr or luvus, not {:?}",
                setting
            )),
        };
    }
    let found = ask.markers.found();
    match found.as_slice() {
        [one] => Ok(Chosen {
            mux: *one,
            why: format!("{} {} set", one.marker(), if *one == Mux::Rex { "are" } else { "is" }),
        }),
        [] if !ask.rex_server.trim().is_empty() => Ok(Chosen {
            mux: Mux::Rex,
            why: "a Rex server is set in settings, watched from outside Rex".into(),
        }),
        [] => Err(outside()),
        several => {
            let markers = several.iter().map(|m| m.marker()).collect::<Vec<_>>().join(", ");
            let nearest = ask
                .ancestors
                .iter()
                .filter_map(|name| mux_of_process(name))
                .find(|m| several.contains(m));
            Ok(match nearest {
                Some(mux) => Chosen {
                    mux,
                    why: format!(
                        "{} are all set; {} is the nearest multiplexer above this process",
                        markers,
                        mux.name()
                    ),
                },
                // The tree did not name one: say so, and take the one most
                // likely started inside another, so `--mux` is the fix.
                None => {
                    let mux = [Mux::Herdr, Mux::Luvus, Mux::Rex]
                        .into_iter()
                        .find(|m| several.contains(m))
                        .unwrap_or(several[0]);
                    Chosen {
                        mux,
                        why: format!(
                            "{} are all set and the process tree names none of them; took {} - use --mux to choose",
                            markers,
                            mux.name()
                        ),
                    }
                }
            })
        }
    }
}

/// The error outside every multiplexer, before the terminal is taken.
pub fn outside() -> String {
    [
        "panes runs inside Rex, Herdr or Luvus. Open it from a pane in one of those:",
        "  Rex    start rex, then run panes in one of its panes",
        "  Herdr  start herdr, then run panes in one of its panes",
        "  Luvus  start luvus, then run panes in one of its panes",
        "To watch a Rex server from anywhere, set its rex_server in panes' settings.",
        "To choose one regardless of where this runs: panes --mux rex|herdr|luvus",
    ]
    .join("\n")
}

/// `/proc/<pid>/stat`: the command name and the parent pid. The name sits
/// in parentheses and may itself hold spaces and parentheses, so the
/// fields after it are counted from the last `)`.
/// Read on one platform, compiled and tested on every one.
#[allow(dead_code)]
pub fn parse_stat_parent(text: &str) -> Option<(String, i64)> {
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    let name = text.get(open + 1..close)?.to_string();
    let mut rest = text.get(close + 1..)?.split_whitespace();
    let _state = rest.next()?;
    let ppid = rest.next()?.parse().ok()?;
    Some((name, ppid))
}

/// `ps -o ppid=,comm= -p <pid>`: the parent pid and the command, which on
/// macOS is the full path.
/// Read on one platform, compiled and tested on every one.
#[allow(dead_code)]
pub fn parse_ps_parent(text: &str) -> Option<(String, i64)> {
    let line = text.lines().find(|l| !l.trim().is_empty())?.trim();
    let (ppid, name) = line.split_once(char::is_whitespace)?;
    Some((name.trim().to_string(), ppid.trim().parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Markers {
        let pairs: Vec<(String, String)> =
            pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        Markers::read(|name| pairs.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()))
    }

    fn ask<'a>(markers: Markers, ancestors: &'a [String]) -> Ask<'a> {
        Ask { flag: None, setting: "", rex_server: "", markers, ancestors }
    }

    #[test]
    fn each_marker_alone_picks_its_multiplexer() {
        let rex = env(&[("REX_SESSION", "s1"), ("REX_BLOCK", "b1"), ("REX_SERVER", "unix:/x")]);
        assert_eq!(choose(&ask(rex, &[])).unwrap().mux, Mux::Rex);
        assert_eq!(choose(&ask(env(&[("HERDR_ENV", "1")]), &[])).unwrap().mux, Mux::Herdr);
        assert_eq!(choose(&ask(env(&[("LUVUS_ENV", "1")]), &[])).unwrap().mux, Mux::Luvus);
    }

    #[test]
    fn half_a_marker_is_no_marker() {
        // REX_SESSION alone is a variable someone exported, not a Rex pane.
        assert_eq!(env(&[("REX_SESSION", "s1")]), Markers::default());
        assert_eq!(env(&[("HERDR_ENV", "0")]), Markers::default());
        assert_eq!(env(&[("REX_SESSION", ""), ("REX_BLOCK", "b")]), Markers::default());
    }

    #[test]
    fn outside_every_multiplexer_is_an_error_naming_all_three() {
        let why = choose(&ask(Markers::default(), &[])).unwrap_err();
        for name in ["Rex", "Herdr", "Luvus", "--mux"] {
            assert!(why.contains(name), "{why}");
        }
    }

    #[test]
    fn a_configured_rex_server_counts_as_inside_rex() {
        let a = Ask { rex_server: "studio", ..ask(Markers::default(), &[]) };
        let chosen = choose(&a).unwrap();
        assert_eq!(chosen.mux, Mux::Rex);
        assert!(chosen.why.contains("Rex server is set"), "{}", chosen.why);
        // But a pane's own marker still decides when there is one.
        let a = Ask { rex_server: "studio", ..ask(env(&[("HERDR_ENV", "1")]), &[]) };
        assert_eq!(choose(&a).unwrap().mux, Mux::Herdr);
    }

    #[test]
    fn several_markers_are_settled_by_the_nearest_multiplexer_above() {
        // Herdr started inside a Rex pane: every Herdr pane inherits REX_*.
        let both = env(&[("REX_SESSION", "s"), ("REX_BLOCK", "b"), ("HERDR_ENV", "1")]);
        let tree: Vec<String> = ["zsh", "herdr", "zsh", "rex"].iter().map(|s| s.to_string()).collect();
        let chosen = choose(&ask(both, &tree)).unwrap();
        assert_eq!(chosen.mux, Mux::Herdr);
        assert!(chosen.why.contains("nearest"), "{}", chosen.why);
        // Rex started inside Herdr: the other way round.
        let tree: Vec<String> = ["/bin/zsh", "/usr/local/bin/rex", "zsh", "herdr"].iter().map(|s| s.to_string()).collect();
        assert_eq!(choose(&ask(both, &tree)).unwrap().mux, Mux::Rex);
        // A tree that names neither still picks, and says to use --mux.
        let chosen = choose(&ask(both, &[])).unwrap();
        assert_eq!(chosen.mux, Mux::Herdr);
        assert!(chosen.why.contains("--mux"), "{}", chosen.why);
    }

    #[test]
    fn the_flag_beats_the_setting_which_beats_the_markers() {
        let herdr = env(&[("HERDR_ENV", "1")]);
        let a = Ask { setting: "luvus", ..ask(herdr, &[]) };
        assert_eq!(choose(&a).unwrap().mux, Mux::Luvus);
        let a = Ask { flag: Some("REX"), setting: "luvus", ..ask(herdr, &[]) };
        assert_eq!(choose(&a).unwrap().mux, Mux::Rex);
        // `auto` and empty mean find out.
        let a = Ask { setting: "Auto", ..ask(herdr, &[]) };
        assert_eq!(choose(&a).unwrap().mux, Mux::Herdr);
        // A word that is no backend is an error, not a silent guess.
        assert!(choose(&Ask { flag: Some("tmux"), ..ask(herdr, &[]) }).is_err());
        assert!(choose(&Ask { setting: "tmux", ..ask(herdr, &[]) }).is_err());
    }

    #[test]
    fn process_parents_are_read_from_proc_and_from_ps() {
        assert_eq!(
            parse_stat_parent("4012 (my (odd) name) S 977 4012 4012 0 -1"),
            Some(("my (odd) name".into(), 977))
        );
        assert_eq!(parse_stat_parent("garbage"), None);
        assert_eq!(parse_ps_parent("  977 /usr/local/bin/rex\n"), Some(("/usr/local/bin/rex".into(), 977)));
        assert_eq!(parse_ps_parent(""), None);
        assert_eq!(mux_of_process("/opt/homebrew/bin/herdr"), Some(Mux::Herdr));
        assert_eq!(mux_of_process("zsh"), None);
    }
}
