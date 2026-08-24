// terminal-toys - small dependency-free terminal widgets
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

//! check.py, for the Rust side.
//!
//! Every check in check.py exists because something shipped broken, and
//! each is a fault that looks on screen exactly like "there is no data".
//! check.py reads only `*.py`, so none of them has ever run against these
//! fourteen binaries - and two of them fail today.
//!
//! These are tests rather than a fifteenth binary for two reasons: they
//! then run on every `cargo test` instead of waiting to be remembered, and
//! `start`'s menu asserts that every `[[bin]]` in the manifest is on it, so
//! a checker binary would have to be listed as a widget it is not.
//!
//! Two of check.py's five do not need porting and are recorded here rather
//! than silently dropped:
//!
//! - **unbound names**: a compile error in Rust. The fault it was written
//!   for - deployments.py losing an import and its poll thread dying for a
//!   day - cannot reach a built binary.
//! - **missing docs and README rows**: the docs are shared between the two
//!   implementations and check.py already covers them. Re-checking here
//!   would only duplicate it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

/// The repo root, from this crate's own location.
fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repo root")
}

/// Every widget binary, by stem, with its source.
fn widgets() -> BTreeMap<String, String> {
    let dir = root().join("rust/widgets/src/bin");
    let mut found = BTreeMap::new();
    for entry in std::fs::read_dir(&dir).expect("the bin directory").flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
        let mut src = std::fs::read_to_string(&path).unwrap_or_default();
        // A widget split across a directory - usage - reads as one widget.
        let sub = dir.join(&stem);
        if sub.is_dir() {
            for part in std::fs::read_dir(&sub).expect("a widget directory").flatten() {
                if part.path().extension().and_then(|e| e.to_str()) == Some("rs") {
                    src.push('\n');
                    src.push_str(&std::fs::read_to_string(part.path()).unwrap_or_default());
                }
            }
        }
        found.insert(stem, src);
    }
    found
}

/// Text inside double-quoted string literals, where hints live.
///
/// Deliberately crude: it is looking for `[w]indow`, and a hint never
/// spans a line. Parsing Rust properly to find a footer would be a much
/// larger thing that failed in more interesting ways.
fn string_lines(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in src.lines() {
        let mut found = String::new();
        let mut rest = line;
        while let Some(open) = rest.find('"') {
            let after = &rest[open + 1..];
            match after.find('"') {
                Some(close) => {
                    found.push_str(&after[..close]);
                    found.push(' ');
                    rest = &after[close + 1..];
                }
                None => break,
            }
        }
        if !found.trim().is_empty() {
            out.push(found);
        }
    }
    out
}

/// The glyphs a hint uses instead of a name, and what they answer to.
///
/// The control standard leans on these four, so a hint that names a key
/// only as an arrow or a return symbol is still a hint teaching a key.
const GLYPHS: &[(char, &str)] = &[
    ('\u{21b5}', "enter"), // ↵
    ('\u{2192}', "right"), // →
    ('\u{2190}', "left"),  // ←
    ('\u{2191}', "up"),    // ↑
    ('\u{2193}', "down"),  // ↓
];

/// Named keys that appear in prose: "esc, ↵ or i to close".
const NAMED: &[&str] = &["esc", "tab", "enter", "backspace", "pgup", "pgdn", "home", "end"];

/// The keys a hint teaches, in any of the forms this tree writes them.
///
/// `[w]indow` and `[d] cloudflare` and `[±]1min` and `↵ starts one` and
/// `esc, ↵ or i to close` are all hints; `"[{}]"`, `"[::1]:"`, `"[[bin]]"`
/// and `"args[0]"` are all strings that merely contain brackets. The
/// separation is four rules, each of which exists for one of those:
///
/// - exactly one character between the brackets - kills `[::1]` and `[[bin]]`
/// - that character is not `{` - kills the format placeholder `[{}]`
/// - the character before `[` is not alphanumeric - kills `args[0]`
/// - the character after `]` is not `.` or `(` - kills `][0].as_str()`
///
/// An earlier version required a letter immediately after `]`, which threw
/// away every hint with a space in it.
fn hinted_keys(src: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for text in string_lines(src) {
        let chars: Vec<char> = text.chars().collect();
        // A footer separates its hints with `·` AND names at least one key
        // unmistakably - in brackets, as a glyph, or by name. The
        // separator alone is not enough: status lines use it too, and
        // " {} targets · {:.1}s interval · " would otherwise offer `s` as
        // a key, and "last {}d · " would offer `d`.
        let names_a_key = text.contains('[')
            || GLYPHS.iter().any(|(g, _)| text.contains(*g))
            || NAMED.iter().any(|n| {
                text.to_lowercase()
                    .split(|c: char| !c.is_ascii_alphabetic())
                    .any(|w| w == *n)
            });
        let is_footer = text.contains('\u{b7}') && names_a_key;
        for i in 0..chars.len() {
            if chars[i] == '[' && i + 2 < chars.len() && chars[i + 2] == ']' {
                let key = chars[i + 1];
                let before_ok = i == 0 || !chars[i - 1].is_ascii_alphanumeric();
                let after_ok = chars
                    .get(i + 3)
                    .is_none_or(|c| *c != '.' && *c != '(' && *c != '[');
                if key != '{' && before_ok && after_ok {
                    match GLYPHS.iter().find(|(g, _)| *g == key) {
                        // [↵] means the same as a bare ↵.
                        Some((_, name)) => {
                            found.insert((*name).to_string());
                        }
                        // [±] is one glyph standing for a pair of arms,
                        // "+" | "=" and "-" | "_". clocks writes it that
                        // way because the footer has room for one hint and
                        // the widget has two keys.
                        None if key == '\u{b1}' => {
                            found.insert("+".into());
                            found.insert("-".into());
                        }
                        None => {
                            found.insert(key.to_lowercase().to_string());
                        }
                    }
                }
            }
            if is_footer {
                if let Some((_, name)) = GLYPHS.iter().find(|(g, _)| *g == chars[i]) {
                    found.insert((*name).to_string());
                }
            }
        }
        // A key named in prose - "esc, ↵ or i to close" - counts only in a
        // footer. This is the form that matters most: it is where the
        // control standard puts its hints, and a key removed while its
        // prose hint stayed is exactly what this check is for.
        if is_footer {
            let lowered = text.to_lowercase();
            for word in lowered.split(|c: char| !c.is_ascii_alphanumeric()) {
                if NAMED.contains(&word) || word.chars().count() == 1 {
                    found.insert(word.to_string());
                }
            }
        }
    }
    found.remove("");
    found
}

/// The keys a match arm answers to, single characters and named alike.
///
/// Case-insensitive on purpose: arms read `"q" | "Q"`, and a pattern that
/// only matched lowercase would fail the whole alternation on the
/// uppercase half - which is exactly the bug that made an earlier hand-run
/// of this rule report 48 failures against widgets that were all correct.
fn handled_keys(src: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for line in src.lines() {
        if line.trim_start().starts_with("//") {
            continue;
        }
        // A match arm, up to its =>; or a comparison anywhere on the line.
        // ports answers `f` with `if key == "f"` and `y` with
        // `if key != "y"`, and neither is an arm.
        let head = match line.find("=>") {
            Some(at) => &line[..at],
            None if line.contains("key ==") || line.contains("key !=") => line,
            None => continue,
        };
        if !head.contains('"') {
            continue;
        }
        let mut rest = head;
        while let Some(open) = rest.find('"') {
            let after = &rest[open + 1..];
            let Some(close) = after.find('"') else { break };
            let word = &after[..close];
            // Any single character counts, punctuation included: "?" and
            // "/" are real keys. Longer words count when they name one.
            let single = word.chars().count() == 1;
            if single || word.chars().all(|c| c.is_ascii_alphabetic()) {
                found.insert(word.to_lowercase());
            }
            rest = &after[close + 1..];
        }
    }
    // A guard rather than a literal: `digit if digit.chars().all(is_ascii_digit)`
    // answers every digit and leaves no "1" to find.
    if src.contains("is_ascii_digit()") && src.contains(" if ") {
        for d in '0'..='9' {
            found.insert(d.to_string());
        }
    }
    found
}

/// The config section a widget declares, and the keys it reads from it.
fn config_use(src: &str) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut sections = BTreeSet::new();
    let mut keys = BTreeSet::new();
    for line in src.lines() {
        if let Some(at) = line.find("load_config(\"") {
            let after = &line[at + 13..];
            if let Some(end) = after.find('"') {
                sections.insert(after[..end].to_string());
            }
        }
        // cfg_f64(&cfg, "key", ...), cfg_str(cfg, "key", ...), and the
        // direct cfg.get("key") that several widgets use for bools.
        //
        // The receiver matters: a bare `.get("` also matches every JSON
        // lookup in the file - clocks reading its own state file, link
        // parsing `ss` output, usage reading token counts - none of which
        // is config. That produced 24 false failures on the first run.
        for marker in ["cfg_f64(", "cfg_usize(", "cfg_str(", "cfg_strings(", "cfg.get("] {
            let mut from = 0;
            while let Some(at) = line[from..].find(marker) {
                let start = from + at + marker.len();
                if let Some(open) = line[start..].find('"') {
                    let after = &line[start + open + 1..];
                    if let Some(end) = after.find('"') {
                        let key = &after[..end];
                        if !key.is_empty()
                            && key.chars().all(|c| c.is_ascii_lowercase() || c == '_')
                        {
                            keys.insert(key.to_string());
                        }
                    }
                }
                from = start;
            }
        }
    }
    (sections, keys)
}

/// The example config, as section -> keys.
fn example() -> BTreeMap<String, BTreeSet<String>> {
    let text = std::fs::read_to_string(root().join("config.example.json"))
        .expect("config.example.json");
    let parsed: serde_json::Value = serde_json::from_str(&text).expect("valid json");
    let mut out = BTreeMap::new();
    for (section, body) in parsed.as_object().expect("an object") {
        if section.starts_with('_') {
            continue;
        }
        let keys = body
            .as_object()
            .map(|o| o.keys().filter(|k| !k.starts_with('_')).cloned().collect())
            .unwrap_or_default();
        out.insert(section.clone(), keys);
    }
    out
}

#[test]
fn every_footer_hint_names_a_key_the_widget_answers_to() {
    // A hint bound to nothing is worse than a missing feature: it says the
    // feature is there. This is the check that caught four shell ports.
    let mut wrong = Vec::new();
    for (name, src) in widgets() {
        let handled = handled_keys(&src);
        for key in hinted_keys(&src) {
            if !handled.contains(&key) {
                wrong.push(format!("{}: [{}] is hinted, no match arm answers it", name, key));
            }
        }
    }
    assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
}

#[test]
fn every_footer_hint_is_in_the_widgets_doc() {
    // The other direction of the same rule: a documented key that does not
    // exist teaches a lie, and so does an undocumented one that works.
    let mut wrong = Vec::new();
    for (name, src) in widgets() {
        let doc = root().join("docs").join(format!("{}.md", name));
        let Ok(text) = std::fs::read_to_string(&doc) else {
            continue; // matrix is decorative and deliberately undocumented
        };
        for key in hinted_keys(&src) {
            // The docs write these as the glyph, the footers as the name:
            // a table row reads `↵` where the code answers to "enter".
            // Either spelling documents the key.
            let glyph = GLYPHS
                .iter()
                .find(|(_, n)| *n == key)
                .map(|(g, _)| format!("`{}`", g));
            let documented = text.contains(&format!("`{}`", key))
                || glyph.is_some_and(|g| text.contains(&g));
            if !documented {
                wrong.push(format!("{}: [{}] in the footer, not in docs/{}.md", name, key, name));
            }
        }
    }
    assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
}

#[test]
fn every_key_a_widget_reads_is_in_the_example() {
    // The direction check.py does NOT check, in either language. All
    // eleven pomodoro_* keys were read by clocks.py and absent from
    // config.example.json since before the port, so breaks were
    // configurable the whole time and undiscoverable. CLAUDE.md asks for
    // new keys to be added in the same commit; nothing was watching.
    let example = example();
    let mut wrong = Vec::new();
    for (name, src) in widgets() {
        let (sections, keys) = config_use(&src);
        for key in keys {
            let known = sections
                .iter()
                .any(|s| example.get(s).is_some_and(|ks| ks.contains(&key)));
            if !known && !sections.is_empty() {
                wrong.push(format!(
                    "{}: reads {:?} from {:?}, which config.example.json does not document",
                    name, key, sections
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
}

#[test]
fn every_section_in_the_example_is_read_by_the_widget_it_names() {
    // check.py's dead-key check, pointed at the Rust. A section nobody
    // reads is a lie in a sample file: it invites someone to set something
    // and watch nothing happen.
    let example = example();
    let widgets = widgets();
    let mut wrong = Vec::new();
    for section in example.keys() {
        // Sections are named for widgets, with _ where the file has -.
        let stem = section.replace('_', "-");
        let Some(src) = widgets.get(section).or_else(|| widgets.get(&stem)) else {
            continue; // a section for something that is not a Rust widget
        };
        let (declared, _) = config_use(src);
        if !declared.contains(section) {
            wrong.push(format!(
                "config.example.json has a {:?} section and {}.rs never calls load_config for it",
                section, stem
            ));
        }
    }
    assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
}

#[test]
fn every_config_read_falls_back_to_a_code_default() {
    // A key deleted from config.json must land on the widget's own
    // default, not on zero and not on a panic. cfg_f64 and its siblings
    // take a fallback by signature; a bare cfg.get() does not, so those
    // are the ones that can go wrong.
    //
    // Checked live as well as here: clocks with no config file, with no
    // clocks section, with an empty section, and with only the focus key
    // set, all show the code's own durations.
    let mut wrong = Vec::new();
    for (name, src) in widgets() {
        let mut from = 0;
        while let Some(at) = src[from..].find(".get(\"") {
            let start = from + at;
            from = start + 5;
            // Only reads of the config value itself; every other .get() in
            // these files is a JSON lookup on something else.
            let before = &src[start.saturating_sub(40)..start];
            if !before.trim_end().ends_with("cfg") && !before.trim_end().ends_with("&cfg") {
                continue;
            }
            let key: String = src[from..]
                .chars()
                .take_while(|c| *c != '"')
                .collect();
            // The statement this read belongs to, not a fixed window: an
            // earlier hand-audit used 260 characters and wrongly flagged
            // work_days, whose fallback is simply further along.
            let rest = &src[start..];
            let stop = rest.find(';').unwrap_or(rest.len());
            let statement = &rest[..stop];
            let guarded = statement.contains("unwrap_or")
                || statement.contains("unwrap_or_else")
                || statement.contains("unwrap_or_default");
            if !guarded {
                wrong.push(format!(
                    "{}: reads {:?} from config with no fallback in the statement",
                    name, key
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
}

#[test]
fn a_poller_that_dies_records_why() {
    // CLAUDE.md's central gotcha: a thread that stops takes its
    // explanation with it, and an empty pane is indistinguishable from a
    // source with nothing in it. Any widget that spawns a thread must have
    // somewhere to put the reason.
    let mut wrong = Vec::new();
    for (name, src) in widgets() {
        if !src.contains("thread::spawn") {
            continue;
        }
        let records = src.contains("err =")
            || src.contains(".err =")
            || src.contains("why =")
            || src.contains("catch_unwind");
        if !records {
            wrong.push(format!(
                "{}: spawns a poll thread with nowhere to record why it stopped",
                name
            ));
        }
    }
    assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
}
