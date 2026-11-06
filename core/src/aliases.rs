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

//! Other names a widget answers to.
//!
//! A widget that is renamed, or that replaces others, keeps the old names
//! working: `opscope herdr-panes` starts `mux-activity`, `opscope
//! deployments` starts `vercel-deployments`. Each widget declares its own
//! aliases where it is registered with the launcher, as an array, and this
//! is the one place a name is turned into a widget, so nothing else can
//! disagree about what a name means.
//!
//! An alias is a name only. It starts the widget exactly as its own name
//! would; a widget that wants to behave differently under an old name reads
//! its arguments, not which name it was called by.

/// The widget a command-line name refers to: its own name, or one of the
/// aliases it declares. `.py` is accepted on either, because every widget
/// answered to its script name for years and the habit outlives the files.
///
/// `widgets` is `(name, aliases)` for every widget; the name returned is
/// one of those names, never the alias.
pub fn resolve_widget<'a>(asked: &str, widgets: &[(&'a str, &[&str])]) -> Option<&'a str> {
    let asked = asked.strip_suffix(".py").unwrap_or(asked);
    widgets
        .iter()
        .find(|(name, _)| *name == asked)
        .or_else(|| widgets.iter().find(|(_, aliases)| aliases.contains(&asked)))
        .map(|(name, _)| *name)
}

/// What is wrong with a set of alias declarations, one line each; empty
/// when nothing is.
///
/// An alias that is another widget's name would hide that widget, and one
/// claimed twice would start whichever was registered first, so both are
/// refused. Names are lowercase words joined by hyphens, like the widgets'
/// own, because they are typed at a prompt and installed beside them.
pub fn alias_problems(widgets: &[(&str, &[&str])]) -> Vec<String> {
    let mut wrong = Vec::new();
    let mut seen: Vec<(&str, &str)> = Vec::new();
    for (name, aliases) in widgets {
        for alias in aliases.iter() {
            let shaped = !alias.is_empty()
                && !alias.starts_with('-')
                && !alias.ends_with('-')
                && alias.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
            if !shaped {
                wrong.push(format!("{name}: alias {alias:?} is not lowercase words joined by hyphens"));
            }
            if widgets.iter().any(|(other, _)| other == alias) {
                wrong.push(format!("{name}: alias {alias:?} is the name of a widget, which it would hide"));
            }
            if let Some((owner, _)) = seen.iter().find(|(_, taken)| taken == alias) {
                wrong.push(format!("{name}: alias {alias:?} is already an alias of {owner}"));
            }
            seen.push((name, alias));
        }
    }
    wrong
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIDGETS: &[(&str, &[&str])] = &[
        ("mux-activity", &["rex-panes", "herdr-panes", "luvus-panes"]),
        ("vercel-deployments", &["deployments"]),
        ("latency", &[]),
    ];

    #[test]
    fn a_name_or_any_alias_finds_its_widget() {
        assert_eq!(resolve_widget("mux-activity", WIDGETS), Some("mux-activity"));
        for old in ["rex-panes", "herdr-panes", "luvus-panes"] {
            assert_eq!(resolve_widget(old, WIDGETS), Some("mux-activity"), "{old}");
        }
        assert_eq!(resolve_widget("deployments", WIDGETS), Some("vercel-deployments"));
        assert_eq!(resolve_widget("deployments.py", WIDGETS), Some("vercel-deployments"));
        assert_eq!(resolve_widget("latency.py", WIDGETS), Some("latency"));
        assert_eq!(resolve_widget("tmux-panes", WIDGETS), None);
        assert_eq!(resolve_widget("", WIDGETS), None);
    }

    #[test]
    fn a_widget_name_wins_over_an_alias_of_the_same_spelling() {
        // Refused by alias_problems; still resolved to the widget, never
        // away from it, if one slips through.
        let clash: &[(&str, &[&str])] = &[("a", &["b"]), ("b", &[])];
        assert_eq!(resolve_widget("b", clash), Some("b"));
    }

    #[test]
    fn colliding_or_misshapen_aliases_are_refused() {
        assert!(alias_problems(WIDGETS).is_empty());
        let shadow: &[(&str, &[&str])] = &[("a", &["b"]), ("b", &[])];
        assert!(alias_problems(shadow)[0].contains("name of a widget"));
        let twice: &[(&str, &[&str])] = &[("a", &["old"]), ("b", &["old"])];
        assert!(alias_problems(twice)[0].contains("already an alias of a"));
        for bad in ["Old", "old_name", "-old", "old-", "", "old name"] {
            let one: &[(&str, &[&str])] = &[("a", &[bad])];
            assert!(!alias_problems(one).is_empty(), "{bad:?}");
        }
    }
}
