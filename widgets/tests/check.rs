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

//! check.py, for the Rust side.
//!
//! Every check in check.py exists because something shipped broken, and
//! each is a fault that looks on screen exactly like "there is no data".
//! check.py reads only `*.py`, so none of them has ever run against these
//! fourteen binaries - and two of them fail today.
//!
//! These are tests rather than a fifteenth binary for two reasons: they
//! then run on every `cargo test` instead of waiting to be remembered, and
//! `opscope`'s menu asserts that every `[[bin]]` in the manifest is on it, so
//! a checker binary would have to be listed as a widget it is not.
//!
//! One of check.py's five does not need porting and is recorded here rather
//! than silently dropped:
//!
//! - **unbound names**: a compile error in Rust. The fault it was written
//!   for - deployments.py losing an import and its poll thread dying for a
//!   day - cannot reach a built binary.
//!
//! Missing docs and README rows used to be left to check.py. That file went
//! with the Python, and a rename is exactly when the gap bites: the help
//! text and the doc page move with the source stem, but the README tables,
//! the docs index, and a name in the launcher's sample listing do not.
//! Those are checked here now. The launcher's own `WIDGETS` list is
//! asserted from `opscope.rs` against every `[[bin]]` in the manifest.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

/// The repo root, from this crate's own location.
fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .canonicalize()
        .expect("the repo root")
}

/// A moved helper declaration, after indent and an optional `pub`.
///
/// The first version of this check compared raw lines against prefixes
/// beginning `fn` / `const`, so an indented copy or a `pub fn now(` sat
/// unnoticed. Those are the two shapes a reintroduced helper actually
/// takes — nested inside an `impl`/`mod`, or published from the widget.
fn is_moved_helper(line: &str, definition: &str) -> bool {
    let line = line.trim_start();
    let line = line.strip_prefix("pub ").unwrap_or(line);
    line.starts_with(definition)
}

#[test]
fn moved_helper_matcher_sees_indent_and_pub() {
    // The column-zero, private prefixes the first check used.
    assert!(is_moved_helper("fn now() -> i64 {", "fn now("));
    assert!(is_moved_helper(
        "const SPARK: [&str; 8] = [",
        "const SPARK:"
    ));
    // The two shapes it missed: indent, and `pub`.
    assert!(is_moved_helper("    fn now() -> i64 {", "fn now("));
    assert!(is_moved_helper("pub fn now() -> i64 {", "fn now("));
    assert!(is_moved_helper(
        "    pub const SPARK: [&str; 8] = [",
        "const SPARK:"
    ));
    // A call is not a definition.
    assert!(!is_moved_helper("let now = tc::now();", "fn now("));
    assert!(!is_moved_helper("tc::run_quiet(&cmd)", "fn run_quiet("));
}

#[test]
fn shared_helpers_are_not_redefined_by_widgets() {
    let moved = [
        "fn now(",
        "fn run(",
        "fn run_quiet(",
        "fn overlay(",
        "const SPARK:",
        "const BRAILLE:",
        "const SPINNER:",
    ];
    let mut wrong = Vec::new();
    for (name, src) in widgets() {
        for definition in moved {
            if src.lines().any(|line| is_moved_helper(line, definition)) {
                wrong.push(format!("{name}: still defines `{definition}`"));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "shared helpers copied back into widgets:\n{}",
        wrong.join("\n")
    );
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
///
/// A widget lives in one of two places: `src/widgets/<name>/` (the package
/// folder, with `main.rs` beside its modules) or `src/bin/<name>.rs` (the
/// layout that has not moved yet). Packages win when both exist.
fn widgets() -> BTreeMap<String, String> {
    let mut found = BTreeMap::new();
    let packages = root().join("widgets/src/widgets");
    if packages.is_dir() {
        for entry in std::fs::read_dir(&packages)
            .expect("the widgets directory")
            .flatten()
        {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let stem = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            if !path.join("main.rs").is_file() {
                continue;
            }
            found.insert(stem, concat_rs(&path));
        }
    }
    let dir = root().join("widgets/src/bin");
    for entry in std::fs::read_dir(&dir)
        .expect("the bin directory")
        .flatten()
    {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        if found.contains_key(&stem) {
            continue;
        }
        // Each file's own tests are dropped before joining, not after. Split
        // on the first `#[cfg(test)]` of the joined blob and a widget with
        // submodules is read only as far as its main file's tests - agent-usage's
        // are two thirds of the way down, so its eight submodules, seven
        // thousand lines, were invisible to every check that did it that way.
        let mut src = without_tests(&std::fs::read_to_string(&path).unwrap_or_default());
        let sub = dir.join(&stem);
        if sub.is_dir() {
            src.push('\n');
            src.push_str(&concat_rs(&sub));
        }
        found.insert(stem, src);
    }
    found
}

fn concat_rs(dir: &std::path::Path) -> String {
    let mut src = String::new();
    for part in std::fs::read_dir(dir)
        .expect("a widget directory")
        .flatten()
    {
        if part.path().extension().and_then(|e| e.to_str()) == Some("rs") {
            src.push('\n');
            src.push_str(&without_tests(
                &std::fs::read_to_string(part.path()).unwrap_or_default(),
            ));
        }
    }
    src
}

fn widget_package_dir(name: &str) -> Option<PathBuf> {
    let dir = root().join("widgets/src/widgets").join(name);
    dir.join("main.rs").is_file().then_some(dir)
}

fn widget_help_path(name: &str) -> PathBuf {
    match widget_package_dir(name) {
        Some(dir) => dir.join("help.txt"),
        None => root()
            .join("widgets/src/bin")
            .join(format!("{name}_help.txt")),
    }
}

fn widget_macos_path(name: &str) -> PathBuf {
    match widget_package_dir(name) {
        Some(dir) => dir.join("macos.rs"),
        None => root().join(format!("widgets/src/bin/{name}/macos.rs")),
    }
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
const NAMED: &[&str] = &[
    "esc",
    "tab",
    "enter",
    "backspace",
    "pgup",
    "pgdn",
    "home",
    "end",
];

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
/// Rejoin a wrapped method chain onto the line it belongs to.
///
/// `cfg.get("show_hints")` and the same call split over two lines are one
/// read, and rustfmt picks between them on line width alone - so a scanner
/// working a line at a time sees the first and is blind to the second.
/// Two real settings, clocks' `show_hints` and `work_days`, sat
/// undocumented behind exactly that shape while this check reported all
/// clear. A blind spot in a check that exists to find undocumented
/// settings is worse than no check, because it is read as proof.
///
/// Only a newline whose next non-space character is `.` is removed, which
/// is narrow enough that nothing else on either line changes meaning.
/// Widening the receiver instead - accepting `raw.get(` and `gh.get(` as
/// well as `cfg.get(` - was tried and reverted: it picks up `cwnd` and
/// `reord_seen`, which are fields link reads out of `ss` output and not
/// config at all.
fn join_chains(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    for line in src.lines() {
        if line.trim_start().starts_with('.') && !out.is_empty() {
            out.push_str(line.trim_start());
        } else {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(line);
        }
    }
    out
}

fn config_use(src: &str) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut sections = BTreeSet::new();
    let mut keys = BTreeSet::new();
    let joined = join_chains(src);
    for line in joined.lines() {
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
        // parsing `ss` output, agent-usage reading token counts - none of which
        // is config. That produced 24 false failures on the first run.
        for marker in [
            "cfg_f64(",
            "cfg_usize(",
            "cfg_str(",
            "cfg_strings(",
            "cfg.get(",
        ] {
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
    let text =
        std::fs::read_to_string(root().join("config.example.json")).expect("config.example.json");
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
                wrong.push(format!(
                    "{}: [{}] is hinted, no match arm answers it",
                    name, key
                ));
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
            let documented =
                text.contains(&format!("`{}`", key)) || glyph.is_some_and(|g| text.contains(&g));
            if !documented {
                wrong.push(format!(
                    "{}: [{}] in the footer, not in docs/{}.md",
                    name, key, name
                ));
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
            let key: String = src[from..].chars().take_while(|c| *c != '"').collect();
            // The statement this read belongs to, not a fixed window: an
            // earlier hand-audit used 260 characters and wrongly flagged
            // work_days, whose fallback is simply further along.
            let rest = &src[start..];
            let stop = rest.find(';').unwrap_or(rest.len());
            let statement = &rest[..stop];
            let guarded = statement.contains("unwrap_or")
                || statement.contains("unwrap_or_else")
                || statement.contains("unwrap_or_default")
                // Asking whether a key is *there* has no value to default.
                // clocks needs it: a `cities` list that was set and came to
                // nothing must stay nothing, where an absent one gets the
                // code's four. Collapsing those two is the bug, and the
                // fallback this rule asks for is what would collapse them.
                || statement.contains(".is_some()")
                || statement.contains(".is_none()");
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
fn every_widget_has_a_readme_row() {
    // check.py used to hold this, and the launcher's doc still says it does.
    // A rename that updates the source stem and forgets the table is how a
    // widget ships without a way to find it.
    let readme = std::fs::read_to_string(root().join("README.md")).expect("README.md");
    let mut wrong = Vec::new();
    for name in widgets().keys() {
        let cell = format!("**`{}`**", name);
        if !readme.contains(&cell) {
            wrong.push(format!("{}: no README.md table row", name));
        }
    }
    assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
}

#[test]
fn every_documented_widget_is_in_the_docs_index() {
    let index = std::fs::read_to_string(root().join("docs/README.md")).expect("docs/README.md");
    let mut wrong = Vec::new();
    for name in widgets().keys() {
        let doc = root().join("docs").join(format!("{}.md", name));
        if !doc.exists() {
            continue; // matrix is decorative and deliberately undocumented
        }
        let cell = format!("[`{}`]({}.md)", name, name);
        if !index.contains(&cell) {
            wrong.push(format!("{}: no docs/README.md row", name));
        }
    }
    assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
}

#[test]
fn widget_names_in_the_launcher_sample_are_current() {
    // The sample listing is not every widget. It is the place a rename
    // forgets: a left-column name that is not a widget is the old name.
    let text = std::fs::read_to_string(root().join("docs/opscope.md")).expect("docs/opscope.md");
    let widgets = widgets();
    let mut wrong = Vec::new();
    let mut in_sample = false;
    for line in text.lines() {
        if line.starts_with("```") {
            if in_sample {
                break; // only the opening listing, not later pictures
            }
            in_sample = true;
            continue;
        }
        if !in_sample {
            continue;
        }
        // A menu row is a stem, then a column of spaces, then the summary.
        // `needs \`ss\`` in the same listing has only one space after the
        // word, and is not a widget.
        let rest = line.trim_start().trim_start_matches("▸ ").trim_start();
        let Some(at) = rest.find(|c: char| c.is_whitespace()) else {
            continue;
        };
        let stem = &rest[..at];
        let pad = rest[at..].chars().take_while(|c| c.is_whitespace()).count();
        if pad < 2
            || stem.is_empty()
            || stem.starts_with('╺')
            || stem == "…"
            || !stem.chars().all(|c| c.is_ascii_lowercase() || c == '-')
        {
            continue;
        }
        if !widgets.contains_key(stem) {
            wrong.push(format!(
                "docs/opscope.md lists {:?} in the sample and no widget is called that",
                stem
            ));
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
        "opens",
        "cycles",
        "toggles",
        "quits",
        "refreshes",
        "closes",
        "copies",
    ];
    let mut wrong = Vec::new();
    for (name, src) in widgets() {
        let help = widget_help_path(&name);
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
                let before_verb = words.get(i + 1).is_some_and(|next| VERBS.contains(next));
                if (after_press || before_verb) && !handled.contains(*word) {
                    wrong.push(format!(
                        "{} names {:?} and {}.rs does not answer it",
                        help.file_name().unwrap_or_default().to_string_lossy(),
                        word,
                        name
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
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
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
            let Some(field) = t.split(':').next() else {
                continue;
            };
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
                let Some(&c) = colours.get(&field) else {
                    continue;
                };
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
    assert!(
        wrong.is_empty(),
        "on the selected-row tint:\n{}",
        wrong.join("\n")
    );
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
    assert!(
        wrong.is_empty(),
        "a lighter grey nobody draws:\n{}",
        wrong.join("\n")
    );
}

/// Widgets that answer neither wheel event, deliberately.
///
/// `matrix` computes nothing and has no list, so there is nothing under a
/// viewport to move. It is a list rather than a heuristic on purpose: a new
/// widget that genuinely does not scroll has to be written down here, which
/// is a decision someone makes in a review, not a gap nobody notices.
const NO_SCROLL: &[&str] = &["matrix"];

#[test]
fn every_widget_answers_the_wheel() {
    // The rule this enforces: the mouse moves the view, keys move the
    // selection. It is written into AGENTS.md and docs/design.md, and it is
    // prose in both - which is exactly what the contrast rule was for as
    // long as there were four widgets to break it. This is the half that
    // fails a build.
    //
    // Every widget, not "every widget that looks like it scrolls". The
    // obvious marker is a call to `follow(`, which the issue proposed and
    // which six of the fifteen scrolling widgets do not use - latency,
    // netwatch, herdr-panes, clocks, agent-usage and github-prs all keep
    // their offset by hand. A check built on it would have passed all six
    // while they answered nothing.
    // Not `src.contains("wheel-up")`: that passes on the word appearing in
    // a comment, and a comment saying the wheel scrolls is the thing this
    // check exists to disbelieve. `handled_keys` cannot be reused as it
    // stands - it keeps single characters and all-alphabetic words, so a
    // hyphenated event name is dropped before it is ever compared - so this
    // reads arm heads and comparisons in the same shape, keeping the
    // literal it is looking for.
    fn answers(src: &str, event: &str) -> bool {
        let quoted = format!("\"{}\"", event);
        src.lines().any(|line| {
            if line.trim_start().starts_with("//") {
                return false;
            }
            let head = match line.find("=>") {
                Some(at) => &line[..at],
                None if line.contains("key ==") || line.contains("key !=") => line,
                None => return false,
            };
            head.contains(&quoted)
        })
    }

    let mut missing: Vec<String> = Vec::new();
    for (name, src) in widgets() {
        if NO_SCROLL.contains(&name.as_str()) {
            continue;
        }
        for event in ["wheel-up", "wheel-down"] {
            if !answers(&src, event) {
                missing.push(format!("{}: never answers {}", name, event));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "the wheel scrolls every widget, or it is not a rule:\n  {}\n\
         Add the event to the widget's key match beside ctrl-y and ctrl-e, \
         moving its viewport offset and nothing else. If the widget really \
         has nothing to scroll, name it in NO_SCROLL above and say why.",
        missing.join("\n  ")
    );
}

#[test]
fn the_wheel_is_turned_off_on_every_way_out() {
    // Three exits, and tracking left on outlives the process: every later
    // click spits escape bytes at the shell prompt, caused by something
    // that has already exited, with nothing on screen to explain it.
    //
    // The signal handler's constant is asserted in core's own tests, where
    // the constant lives. These two are the paths a reader forgets: the
    // normal quit, and the Drop that runs when a widget panics.
    let src = std::fs::read_to_string(root().join("core/src/lib.rs")).expect("core");
    for (what, from) in [
        ("restore_screen", "pub fn restore_screen()"),
        ("Keyboard::restore", "pub fn restore(&mut self)"),
    ] {
        let at = src
            .find(from)
            .unwrap_or_else(|| panic!("{} moved or was renamed", what));
        let body = &src[at..src.len().min(at + 900)];
        assert!(
            body.contains("MOUSE_OFF"),
            "{} does not send MOUSE_OFF. A widget leaving by that path hands \
             back a terminal that is still reporting.",
            what
        );
    }
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

/// Visibility that can hide a `fn parse_*` from a matcher that only
/// strips `pub `.
fn rust_item(t: &str) -> &str {
    let t = t.strip_prefix("pub(crate) ").unwrap_or(t);
    let t = t.strip_prefix("pub(super) ").unwrap_or(t);
    t.strip_prefix("pub ").unwrap_or(t)
}

/// A `#[cfg(target_os ...)]` still pending when the next item starts.
///
/// Attributes stack, so `#[cfg(target_os)]` then `#[path]` then `mod host`
/// is one item; a blank line or a comment does not clear it. The attribute
/// itself may span lines. An inline `mod { ... }` behind the cfg is
/// inspected here; a `mod name;` file is judged by `cfg_gated_mod_files`.
fn parsers_or_tests_gated_by_target_os(src: &str) -> Vec<String> {
    let mut pending = false;
    let mut attr = String::new();
    let mut in_attr = false;
    let mut found = Vec::new();
    let lines: Vec<&str> = src.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let t = lines[i].trim();
        if !in_attr && (t.starts_with("//") || t.is_empty()) {
            i += 1;
            continue;
        }
        if in_attr || t.starts_with("#[cfg(") {
            if !in_attr {
                attr.clear();
            }
            attr.push_str(t);
            let open = attr.chars().filter(|&c| c == '[').count();
            let close = attr.chars().filter(|&c| c == ']').count();
            in_attr = open > close;
            if !in_attr {
                if attr.contains("target_os") {
                    pending = true;
                }
                attr.clear();
            }
            i += 1;
            continue;
        }
        if t.starts_with("#[") {
            if pending && t.starts_with("#[test]") {
                found.push(format!("line {}: test gated by target_os", i + 1));
            }
            i += 1;
            continue;
        }
        if pending {
            let item = rust_item(t);
            if item.starts_with("fn parse_") {
                found.push(format!("line {}: parser gated by target_os", i + 1));
                pending = false;
                i += 1;
                continue;
            }
            if item.starts_with("mod ") && t.contains('{') {
                let mut depth = t.chars().filter(|&c| c == '{').count() as i32
                    - t.chars().filter(|&c| c == '}').count() as i32;
                i += 1;
                while i < lines.len() && depth > 0 {
                    let n = lines[i].trim();
                    depth += n.chars().filter(|&c| c == '{').count() as i32;
                    depth -= n.chars().filter(|&c| c == '}').count() as i32;
                    if rust_item(n).starts_with("fn parse_") {
                        found.push(format!("line {}: parser gated by target_os", i + 1));
                    }
                    if n.starts_with("#[test]") {
                        found.push(format!("line {}: test gated by target_os", i + 1));
                    }
                    i += 1;
                }
                pending = false;
                continue;
            }
            pending = false;
        }
        i += 1;
    }
    found
}

#[test]
fn target_os_gating_sees_a_parser_and_skips_acquisition() {
    let src = r#"
#[cfg(target_os = "linux")]
fn parse_foo(text: &str) {}

#[cfg(target_os = "linux")]
fn sockets() {}

#[cfg(target_os = "macos")]
#[path = "macos.rs"]
mod host;

#[cfg(target_os = "linux")]
#[test]
fn hidden() {}

#[cfg(
    target_os = "linux"
)]
pub(crate) fn parse_bar(text: &str) {}

#[cfg(target_os = "linux")]
mod tests {
    #[test]
    fn also_hidden() {}
    fn parse_inner(text: &str) {}
}
"#;
    let got = parsers_or_tests_gated_by_target_os(src);
    assert!(
        got.iter().any(|s| s.contains("parser")),
        "missed a gated parse_*: {got:?}"
    );
    assert!(
        got.iter().any(|s| s.contains("test")),
        "missed a gated #[test]: {got:?}"
    );
    assert!(
        got.iter().filter(|s| s.contains("parser")).count() >= 2,
        "missed a multiline cfg / pub(crate) parser: {got:?}"
    );
    assert!(
        got.iter().filter(|s| s.contains("test")).count() >= 2,
        "missed an inline cfg-gated test module: {got:?}"
    );
    assert!(
        !got.iter()
            .any(|s| s.contains("sockets") || s.contains("host")),
        "acquisition is allowed behind cfg: {got:?}"
    );
}

/// Every `.rs` file that makes up a widget, including package folders
/// and leftover `src/bin/` files. `widgets()` concatenates those; these
/// checks need the files separately so a `cfg`-gated module can be judged
/// on its own contents.
fn widget_rs_files() -> Vec<(String, PathBuf)> {
    let mut found = Vec::new();
    let packages = root().join("widgets/src/widgets");
    if packages.is_dir() {
        for entry in std::fs::read_dir(&packages)
            .expect("the widgets directory")
            .flatten()
        {
            let path = entry.path();
            if !path.is_dir() || !path.join("main.rs").is_file() {
                continue;
            }
            let stem = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            for part in std::fs::read_dir(&path)
                .expect("a widget directory")
                .flatten()
            {
                if part.path().extension().and_then(|e| e.to_str()) == Some("rs") {
                    found.push((stem.clone(), part.path()));
                }
            }
        }
    }
    let dir = root().join("widgets/src/bin");
    for entry in std::fs::read_dir(&dir)
        .expect("the bin directory")
        .flatten()
    {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            if widget_package_dir(&stem).is_some() {
                continue;
            }
            found.push((stem, path));
            continue;
        }
        if path.is_dir() {
            let stem = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            if widget_package_dir(&stem).is_some() {
                continue;
            }
            for part in std::fs::read_dir(&path)
                .expect("a widget directory")
                .flatten()
            {
                if part.path().extension().and_then(|e| e.to_str()) == Some("rs") {
                    found.push((stem.clone(), part.path()));
                }
            }
        }
    }
    found
}

fn cfg_gated_mod_files(src: &str, file: &PathBuf) -> Vec<PathBuf> {
    let dir = file.parent().expect("a source file has a directory");
    let lines: Vec<&str> = src.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let t = lines[i].trim();
        if t.starts_with("#[cfg(") && t.contains("target_os") {
            let mut path_attr = None;
            let mut j = i + 1;
            while j < lines.len() {
                let n = lines[j].trim();
                if n.starts_with("//") || n.is_empty() {
                    j += 1;
                    continue;
                }
                if n.starts_with("#[path") {
                    if let Some(q1) = n.find('"') {
                        if let Some(q2) = n[q1 + 1..].find('"') {
                            path_attr = Some(n[q1 + 1..q1 + 1 + q2].to_string());
                        }
                    }
                    j += 1;
                    continue;
                }
                if n.starts_with("#[") {
                    j += 1;
                    continue;
                }
                if let Some(rest) = n.strip_prefix("mod ") {
                    let name = rest
                        .trim_end_matches(';')
                        .trim_end_matches('{')
                        .trim()
                        .to_string();
                    out.push(match path_attr {
                        Some(rel) => dir.join(rel),
                        None => dir.join(format!("{name}.rs")),
                    });
                }
                break;
            }
        }
        i += 1;
    }
    out
}

fn file_has_parser_or_test(src: &str) -> bool {
    src.lines().any(|line| {
        let t = line.trim_start();
        if t.starts_with("//") {
            return false;
        }
        let item = rust_item(t);
        item.starts_with("fn parse_") || t.starts_with("#[test]")
    })
}

#[test]
fn parsers_and_their_tests_are_not_gated_by_target_os() {
    // The failure this exists to catch: a Linux parser (or the test that
    // would have exercised it) sitting behind cfg(target_os = "linux"),
    // so cargo test on the macOS runners never compiles it, and a broken
    // decoder sits behind a green build.
    let mut wrong = Vec::new();
    for (widget, path) in widget_rs_files() {
        let src = std::fs::read_to_string(&path).unwrap_or_default();
        for flag in parsers_or_tests_gated_by_target_os(&src) {
            wrong.push(format!(
                "{} {}: {flag}",
                widget,
                path.file_name().unwrap_or_default().to_string_lossy()
            ));
        }
        if let Some(test_at) = src.find("#[cfg(test)]") {
            for (n, line) in src[test_at..].lines().enumerate() {
                let t = line.trim();
                if t.starts_with("#[cfg(") && t.contains("target_os") {
                    wrong.push(format!(
                        "{} {}: test module line {} gated by target_os",
                        widget,
                        path.file_name().unwrap_or_default().to_string_lossy(),
                        n + 1
                    ));
                }
            }
        }
        for gated in cfg_gated_mod_files(&src, &path) {
            let body = std::fs::read_to_string(&gated).unwrap_or_default();
            if file_has_parser_or_test(&body) {
                wrong.push(format!(
                    "{}: {} is cfg-gated but contains a parser or a test",
                    widget,
                    gated.file_name().unwrap_or_default().to_string_lossy()
                ));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "parsers compile on every target, or the tests that prove they do vanish from macOS CI:\n  {}",
        wrong.join("\n  ")
    );
}

#[test]
fn the_linux_socket_parser_is_compiled_on_every_target() {
    // The proof the rule holds for the worked example. If parse_proc_net_tcp
    // moved behind cfg(target_os = "linux"), this file would lose the
    // function (or gain a target_os) and this test would fail on every
    // runner, including macOS — the thing a cfg-gated unit test cannot do.
    let path = root().join("widgets/src/widgets/ports/parse.rs");
    let src = std::fs::read_to_string(&path)
        .expect("ports/parse.rs — the Linux /proc parser lives here so it compiles on macOS too");
    assert!(
        src.contains("fn parse_proc_net_tcp("),
        "the /proc/net/tcp parser must keep its name so this check can see it"
    );
    assert!(
        !src.lines().any(|l| {
            let t = l.trim();
            t.starts_with("#[cfg") && t.contains("target_os")
        }),
        "ports/parse.rs is gated by target_os — its tests would vanish from the macOS CI run"
    );
}

#[test]
fn a_widget_package_is_what_cargo_builds() {
    // The folder is the binary. A package with main.rs that Cargo still
    // points at src/bin/ is two sources, and the one people edit is the
    // one that is not built.
    let manifest =
        std::fs::read_to_string(root().join("widgets/Cargo.toml")).expect("widgets/Cargo.toml");
    let mut wrong = Vec::new();
    for name in widgets().keys() {
        let Some(dir) = widget_package_dir(name) else {
            continue;
        };
        let path = format!("path = \"src/widgets/{name}/main.rs\"");
        if !manifest.contains(&path) {
            wrong.push(format!("{name}: Cargo.toml does not point at {path}"));
        }
        if !dir.join("help.txt").is_file() {
            wrong.push(format!("{name}: package folder missing help.txt"));
        }
        if root()
            .join("widgets/src/bin")
            .join(format!("{name}.rs"))
            .is_file()
        {
            wrong.push(format!("{name}: leftover src/bin/{name}.rs after the move"));
        }
    }
    assert!(
        wrong.is_empty(),
        "a widget package is the thing cargo builds:\n  {}",
        wrong.join("\n  ")
    );
}

fn opens_proc(src: &str) -> bool {
    src.lines().any(|line| {
        let t = line.trim_start();
        if t.starts_with("//") {
            return false;
        }
        // A string that is a /proc path, not a mention of the word in
        // prose. github.rs talks about cmdline leaking; that string does
        // not start with /proc inside the quotes.
        line.contains("\"/proc") || line.contains("format!(\"/proc")
    })
}

#[test]
fn opens_proc_sees_a_path_and_skips_a_mention() {
    assert!(opens_proc(
        r#"let t = std::fs::read_to_string("/proc/net/tcp");"#
    ));
    assert!(opens_proc(r#"format!("/proc/{}/fd", pid)"#));
    assert!(!opens_proc(
        r#"// /proc/<pid>/cmdline is readable by anyone"#
    ));
    assert!(!opens_proc(
        r#""in its arguments, because /proc/<pid>/cmdline is readable by""#
    ));
}

/// Widgets that still acquire from Linux-only sources, and the issue that
/// will give them a macOS path (or decide they cannot). Adding a name
/// here without an issue is the failure this check exists to catch: a
/// new `/proc` reader with no explanation looks on macOS like a source
/// with nothing in it.
const LINUX_ONLY_UNTIL: &[(&str, &str)] = &[
    ("netwatch", "OPS-61"),
    ("tailnet", "process table is /proc; no macOS source yet"),
    (
        "herdr-panes",
        "CPU samples are /proc/<pid>/stat; no macOS source yet",
    ),
    ("agent-usage", "Antigravity's port discovery walks /proc"),
];

#[test]
fn a_proc_reader_has_a_macos_path_or_says_why() {
    let mut wrong = Vec::new();
    let listed: BTreeSet<&str> = LINUX_ONLY_UNTIL.iter().map(|(n, _)| *n).collect();
    for (name, src) in widgets() {
        if !opens_proc(&src) {
            if listed.contains(name.as_str()) {
                wrong.push(format!(
                    "{name}: on LINUX_ONLY_UNTIL but no longer opens /proc — drop it"
                ));
            }
            continue;
        }
        if listed.contains(name.as_str()) {
            continue;
        }
        let macos = widget_macos_path(&name);
        if macos.is_file() {
            let main = match widget_package_dir(&name) {
                Some(dir) => dir.join("main.rs"),
                None => root().join(format!("widgets/src/bin/{name}.rs")),
            };
            let main_src = std::fs::read_to_string(&main).unwrap_or_default();
            if !main_src.contains("macos.rs") && !main_src.contains("mod macos") {
                wrong.push(format!(
                    "{name}: macos.rs is present but main.rs does not load it"
                ));
            }
            continue;
        }
        if src.contains("unsupported(") {
            continue;
        }
        wrong.push(format!(
            "{name}: opens /proc with no macos.rs, no unsupported() call, \
             and no row on LINUX_ONLY_UNTIL"
        ));
    }
    assert!(
        wrong.is_empty(),
        "a widget that reads /proc without a macOS path looks empty on a Mac:\n  {}\n\
         Add widgets/src/widgets/<widget>/macos.rs, call tc::unsupported(), \
         or name it in LINUX_ONLY_UNTIL with the issue that will.",
        wrong.join("\n  ")
    );
}
