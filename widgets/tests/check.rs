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
        .join("..")
        .canonicalize()
        .expect("the repo root")
}

/// Every selection tint a widget composes, and how it was reached.
///
/// Returns `(tint, inline_colour)` - the colour named on the same line when
/// the tint is composed inline, and `None` when it is bound to a `tint`
/// variable and handed to a closure later.
///
/// A whole statement at a time, not a line at a time. herdr-panes binds its
/// tint across nine lines:
///
/// ```ignore
/// let tint = if here {
///     tc::bg(38, 56, 76)
/// } else if a.state == "blocked" {
/// ```
///
/// The line carrying that first `bg()` names no colour and does not contain
/// the word "tint", so a line-by-line scan saw neither and checked nothing
/// for it. It happened not to matter only because the same tint appears on
/// its own line twice more in the same file; alone, or changed on its own, an
/// AA failure there would have gone unseen.
fn tints_of(src: &str) -> Vec<((f64, f64, f64), Option<String>)> {
    let mut found = Vec::new();
    let lines: Vec<&str> = src.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        // A binding whose name is `tint`: take the statement to its end.
        let binds_tint = line.contains("let tint")
            || line.trim_start().starts_with("tint =")
            || line.contains("let tint:");
        if binds_tint {
            let mut depth: i32 = 0;
            let mut j = i;
            loop {
                let l = lines[j];
                depth += l.matches('{').count() as i32 - l.matches('}').count() as i32;
                if let Some(t) = triple(l, "tc::bg") {
                    found.push((t, None));
                }
                // The statement ends at a `;` once every brace has closed.
                if depth <= 0 && l.trim_end().ends_with(';') {
                    break;
                }
                j += 1;
                if j >= lines.len() || j > i + 40 {
                    break;
                }
            }
            i = j + 1;
            continue;
        }
        // Otherwise a bg() composed inline, with the colour on the same line.
        if let Some(t) = triple(line, "tc::bg") {
            found.push((t, Some(line.to_string())));
        }
        i += 1;
    }
    found
}

/// Every `<name>_lit` the palette defines.
fn colours_ending_in_lit(src: &str) -> Vec<String> {
    let mut found = Vec::new();
    for line in src.lines() {
        let t = line.trim_start();
        if !t.contains(": tc::rgb(") {
            continue;
        }
        if let Some(field) = t.split(':').next() {
            if field.ends_with("_lit") && !field.contains(' ') {
                found.push(field.to_string());
            }
        }
    }
    found.sort();
    found.dedup();
    found
}

/// Whether a palette colour is ever handed to the closure that composes a
/// selection tint.
///
/// Most colours are drawn straight - `p.bad.as_str()` - and never meet a
/// tint; blaming those is how a contrast check starts crying wolf, which
/// this file has already had to fix twice. A colour counts if it is named on
/// a line that calls the tint helper, or if it is returned by one of the
/// small colour pickers whose result is then handed to it.
fn handed_to_a_tint(src: &str, field: &str) -> bool {
    let named = format!("p.{}", field);
    let mentions = |line: &str| match line.find(&named) {
        // `p.dim` is a prefix of `p.dim_lit`.
        Some(at) => !line[at + named.len()..].starts_with('_'),
        None => false,
    };
    src.lines().any(|line| {
        let composes = line.contains("c(") || line.contains("c_of(") || line.contains("tinted(");
        let picker = line.trim_start().starts_with("\"") && line.contains("=> &p.");
        (composes && mentions(line)) || (picker && mentions(line))
    })
}

/// A source with its own test module removed. Fixtures are not the screen,
/// and a colour or a key that only appears in one is not shipped.
fn without_tests(src: &str) -> String {
    src.split("#[cfg(test)]").next().unwrap_or("").to_string()
}

/// Every widget binary, by stem, with its source.
fn widgets() -> BTreeMap<String, String> {
    let dir = root().join("widgets/src/bin");
    let mut found = BTreeMap::new();
    for entry in std::fs::read_dir(&dir).expect("the bin directory").flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
        // Each file's own tests are dropped before joining, not after. Split
        // on the first `#[cfg(test)]` of the joined blob and a widget with
        // submodules is read only as far as its main file's tests - usage's
        // are two thirds of the way down, so its eight submodules, seven
        // thousand lines, were invisible to every check that did it that way.
        let mut src = without_tests(&std::fs::read_to_string(&path).unwrap_or_default());
        // A widget split across a directory - usage - reads as one widget.
        let sub = dir.join(&stem);
        if sub.is_dir() {
            for part in std::fs::read_dir(&sub).expect("a widget directory").flatten() {
                if part.path().extension().and_then(|e| e.to_str()) == Some("rs") {
                    src.push('\n');
                    src.push_str(&without_tests(
                        &std::fs::read_to_string(part.path()).unwrap_or_default(),
                    ));
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
fn every_key_in_the_example_is_read_by_the_widget_it_belongs_to() {
    // check.py's rule, per key rather than per section: a key in the
    // example that no widget reads is a lie in a sample file. Checking
    // only that the section is loaded misses the case where a widget
    // reads three of its four keys and ignores the fourth.
    let example = example();
    let widgets = widgets();
    let mut wrong = Vec::new();
    for (section, keys) in &example {
        let stem = section.replace('_', "-");
        let Some(src) = widgets.get(section).or_else(|| widgets.get(&stem)) else {
            continue; // a section for something that is not a Rust widget
        };
        for key in keys {
            // Any mention of the key as a string literal counts. The point
            // is whether the widget knows the name at all, not which
            // helper it reaches for.
            if !src.contains(&format!("\"{}\"", key)) {
                wrong.push(format!(
                    "config.example.json documents {}.{} and {}.rs never reads it",
                    section, key, stem
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
}

#[test]
fn every_key_the_help_text_names_is_answered() {
    // --help is where someone looks when the footer is not enough, so a
    // key named there and not implemented is the same lie as a footer
    // hint bound to nothing. Nothing was reading these files.
    //
    // The help is prose, so there are no brackets to key off. Only two
    // shapes count, both hard to write by accident: a letter right after
    // "press", and a letter right before a verb - "c opens a detail
    // view". Reading every single letter instead took "a" from "with a
    // longer" and reported a key called a.
    //
    // Known limitation, stated rather than hidden: in "Enter, i or c
    // opens", only `c` touches the verb, so a stale `i` beside it is
    // missed. Catching one of the two still lands the reader in the right
    // sentence.
    const VERBS: &[&str] = &[
        "opens", "cycles", "toggles", "quits", "refreshes", "closes", "copies",
    ];
    let dir = root().join("widgets/src/bin");
    let mut wrong = Vec::new();
    for (name, src) in widgets() {
        let help = dir.join(format!("{}_help.txt", name));
        let Ok(text) = std::fs::read_to_string(&help) else {
            continue;
        };
        let handled = handled_keys(&src);
        for line in text.lines() {
            let lower = line.to_lowercase();
            let words: Vec<&str> = lower
                .split(|c: char| !c.is_ascii_alphanumeric())
                .filter(|w| !w.is_empty())
                .collect();
            for (i, word) in words.iter().enumerate() {
                if word.chars().count() != 1 {
                    continue;
                }
                let after_press = i > 0 && words[i - 1] == "press";
                let before_verb = words
                    .get(i + 1)
                    .is_some_and(|next| VERBS.contains(next));
                if (after_press || before_verb) && !handled.contains(*word) {
                    wrong.push(format!(
                        "{}_help.txt names {:?} and {}.rs does not answer it",
                        name, word, name
                    ));
                }
            }
        }
    }
    wrong.sort();
    wrong.dedup();
    assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
}

fn luminance(rgb: (f64, f64, f64)) -> f64 {
    let ch = |c: f64| {
        let c = c / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * ch(rgb.0) + 0.7152 * ch(rgb.1) + 0.0722 * ch(rgb.2)
}

fn contrast(a: (f64, f64, f64), b: (f64, f64, f64)) -> f64 {
    let (x, y) = (luminance(a), luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

/// The three numbers in the first `call(r, g, b)` on a line.
fn triple(line: &str, call: &str) -> Option<(f64, f64, f64)> {
    let at = line.find(call)?;
    let open = line[at..].find('(')? + at;
    let close = line[open..].find(')')? + open;
    let n: Vec<f64> = line[open + 1..close]
        .split(',')
        .filter_map(|x| x.trim().parse().ok())
        .collect();
    (n.len() == 3).then(|| (n[0], n[1], n[2]))
}

/// Text drawn over a selection tint has to clear AA against the tint, not
/// only against the terminal's own background.
///
/// CLAUDE.md has asked for this in prose since before the port, and it went
/// unmet in four widgets for as long as there were four widgets: `dim` at
/// (127, 147, 172) measures 3.81 on `bg(38, 56, 76)`. The count of places it
/// reached a tinted row grew from seventeen to twenty-three while that sat
/// open, which is what prose costs.
///
/// It checks **every** palette colour a widget defines, not the one that was
/// measured first. The version before this one looked only at `dim` and
/// `dim_lit`, and a review found three more failing on the same tint through
/// the same closures - `bad` at 4.17, `unknown` at 4.11, and `idle` at 3.85,
/// which is what most of herdr-panes' rows are. A check that only knows
/// about the bug it was written for finds that bug and stops.
///
/// A colour with a `_lit` twin is exempt: the twin is what reaches the tint,
/// and it is measured instead. Only colours that meet a tint are compared -
/// an earlier version paired every `bg()` in a file with every colour in it
/// and reported two widgets that were fine, one on a tint that exists only
/// in a test fixture. A check that cries wolf gets turned off.
#[test]
fn text_on_a_selection_tint_clears_aa() {
    let mut wrong = Vec::new();
    for (name, src) in widgets() {
        // Every colour the palette defines, by field.
        let mut colours: BTreeMap<String, (f64, f64, f64)> = BTreeMap::new();
        for line in src.lines() {
            let t = line.trim_start();
            let Some(field) = t.split(':').next() else { continue };
            if !t.contains(": tc::rgb(") || field.contains(' ') || field.is_empty() {
                continue;
            }
            if let Some(c) = triple(line, "tc::rgb") {
                colours.insert(field.to_string(), c);
            }
        }
        for (tint, inline_line) in tints_of(&src) {
            // Composed inline with a named colour: that exact pair. Bound to
            // `tint` and handed to a closure: every colour the closure is
            // given can land on it.
            let inline: Vec<String> = match inline_line.as_ref() {
                None => Vec::new(),
                Some(line) => colours
                    .keys()
                    .filter(|f| {
                        // `p.dim` is a prefix of `p.dim_lit`; without the
                        // guard a line drawing only the lighter one would be
                        // blamed for the darker one it never draws.
                        let needle = format!("p.{}", f);
                        match line.find(&needle) {
                            None => false,
                            Some(at) => !line[at + needle.len()..].starts_with('_'),
                        }
                    })
                    .cloned()
                    .collect(),
            };
            let reached: Vec<String> = if !inline.is_empty() {
                inline
            } else if inline_line.is_none() {
                // Not every colour in the palette reaches the tint - most are
                // drawn straight, as `p.bad.as_str()`. Only the ones handed
                // to a closure count, directly or through a colour picker
                // whose arms return them. Without this the check reported
                // github's `bad`, which is only ever drawn on the plain
                // background, and a check that cries wolf gets turned off.
                colours
                    .keys()
                    .filter(|f| handed_to_a_tint(&src, f))
                    .cloned()
                    .collect()
            } else {
                continue;
            };
            for field in reached {
                // Gridlines are not text, and say so where they are defined.
                if field.contains("grid") {
                    continue;
                }
                // The lighter twin is what reaches the tint, so the twin is
                // what gets measured. Skipping both - which this did briefly,
                // and which passed a mutation that put a failing value back
                // into a `_lit` field - measures nothing at all.
                let field = match colours.contains_key(&format!("{}_lit", field)) {
                    true => format!("{}_lit", field),
                    false => field,
                };
                let Some(&c) = colours.get(&field) else { continue };
                let r = contrast(c, tint);
                if r < 4.5 {
                    wrong.push(format!(
                        "{}: {} {:?} on tint {:?} measures {:.2}, under AA 4.5",
                        name, field, c, tint, r
                    ));
                }
            }
        }
    }
    wrong.sort();
    wrong.dedup();
    assert!(wrong.is_empty(), "on the selected-row tint:\n{}", wrong.join("\n"));
}

/// A widget with a lighter grey has to actually reach for it.
///
/// The check above measures the colour and cannot see the wiring: delete the
/// substitution inside the tint closure and `dim` goes back on the tint while
/// `dim_lit` sits in the palette measuring beautifully. So the substitution
/// is counted instead - once per closure that composes a tint.
#[test]
fn a_widget_with_a_lighter_grey_uses_it_on_every_tint() {
    let mut wrong = Vec::new();
    for (name, whole) in widgets() {
        let src = whole.split("#[cfg(test)]").next().unwrap_or("").to_string();
        if !src.contains("dim_lit: tc::rgb") {
            continue;
        }
        let closures = src.matches("format!(\"{}{}\", tint, colour)").count();
        // Every lighter colour the palette defines has to be reached for by
        // every closure that composes a tint. Counting only `dim_lit` let a
        // mutation through: unwiring the `idle_c` arm left the dim_lit count
        // untouched and the check green, which is the shape of hole this
        // test exists to close.
        // A swap and the guard that reaches it come in pairs. Counting
        // occurrences alone cannot see a guard that has been neutered - the
        // body still mentions the lighter colour, so the count is unchanged
        // and the check stays green while the swap never fires.
        for lit in colours_ending_in_lit(&src) {
            let base = lit.trim_end_matches("_lit");
            let swaps = src.matches(&format!("p.{}.as_str()", lit)).count();
            let guards = src.matches(&format!("colour == p.{} {{", base)).count();
            if swaps != guards {
                wrong.push(format!(
                    "{}: {} reached {} times but guarded on `colour == p.{}` {} times",
                    name, lit, swaps, base, guards
                ));
            }
        }
        for lit in colours_ending_in_lit(&src) {
            let swaps = src.matches(&format!("p.{}.as_str()", lit)).count()
                + src.matches(&format!("&p.{}", lit)).count();
            if swaps < closures {
                wrong.push(format!(
                    "{}: {} tint closures but {} reach for {}",
                    name, closures, swaps, lit
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "a lighter grey nobody draws:\n{}", wrong.join("\n"));
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
        // Two accidents used to satisfy this, and both were found by reading
        // the widgets rather than by the check failing.
        //
        // `catch_unwind` on its own counted as recording a reason. It is
        // not: ports caught its panic and threw the reason away with
        // `unwrap_or_default()`, drawing an empty table - the exact thing
        // this rule exists to prevent, under a comment saying so.
        //
        // And a bare `err =` matched `let mut err = dx + dy`, the Bresenham
        // variable in latency's line drawing. That one accident was the
        // whole reason latency passed.
        //
        // So a reason has to look like a reason - assigned something with
        // words in it - and it has to reach a row. Recording one nobody
        // draws is the same silence with more code behind it.
        let reason = |line: &str| {
            // A field called `err` holds a reason whatever it is assigned
            // from - netwatch's `state.err = err` hands over a String built
            // further up. A bare local is where the Bresenham variables
            // live, so that one has to be assigned something with words in
            // it.
            if line.contains(".err =") || line.contains(".why =") {
                return true;
            }
            let Some(at) = line.find("err =").or_else(|| line.find("why =")) else {
                return false;
            };
            let rhs = &line[at..];
            rhs.contains('"') || rhs.contains("format!") || rhs.contains("to_string")
        };
        let records = src
            .lines()
            .any(|l| !l.trim_start().starts_with("//") && reason(l));
        // On screen, not on stderr: netwatch writes one to stderr *and*
        // draws it, and only the drawn one is any use behind a full-screen
        // redraw.
        let drawn = src.contains("err.is_empty()")
            || src.contains("&err")
            || src.contains("why.is_empty()");
        // One shape is always wrong, whatever else the widget records: a
        // caught panic whose reason goes straight to `unwrap_or_default()`.
        // That returns an empty list and the pane draws as if the source had
        // nothing in it. Checked on its own, because "the widget records a
        // reason somewhere" is true of a widget with one good guard and one
        // bad one - which is exactly what ports was.
        for (n, line) in src.lines().enumerate() {
            if line.contains("catch_unwind") && line.contains("unwrap_or_default") {
                wrong.push(format!(
                    "{}:{}: catches a panic and throws the reason away",
                    name,
                    n + 1
                ));
            }
        }
        if !records {
            wrong.push(format!(
                "{}: spawns a poll thread with nowhere to record why it stopped",
                name
            ));
        } else if !drawn {
            wrong.push(format!(
                "{}: records why its poller stopped and never puts it on screen",
                name
            ));
        }
    }
    assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
}
