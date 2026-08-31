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
//! every widget folder has to be on `opscope`'s menu, so a checker binary
//! would have to be listed as a widget it is not.
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
//! Those are checked here now, the launcher's own `WIDGETS` list among
//! them: `every_widget_is_on_the_launcher_menu` reads
//! `widgets/src/launcher/main.rs` and holds it to the widget folders both
//! ways. Two sentences here used to describe that assertion while no such
//! assertion existed - one of them the reason given above - and they named
//! an `opscope.rs` that the port had already replaced.

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
    assert!(is_moved_helper("const SPARK: [&str; 8] = [", "const SPARK:"));
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
fn widgets() -> BTreeMap<String, String> {
    let dir = root().join("widgets/src/widgets");
    let mut found = BTreeMap::new();
    for entry in std::fs::read_dir(&dir).expect("the bin directory").flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let stem = path.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string();
        let main = path.join("main.rs");
        if !main.exists() {
            continue;
        }
        // Each file's own tests are dropped before joining, not after. Split
        // on the first `#[cfg(test)]` of the joined blob and a widget with
        // submodules is read only as far as its main file's tests.
        let mut src = String::new();
        for part in std::fs::read_dir(&path).expect("a widget directory").flatten() {
            if part.path().extension().and_then(|e| e.to_str()) == Some("rs") {
                src.push('\n');
                src.push_str(&without_tests(
                    &std::fs::read_to_string(part.path()).unwrap_or_default(),
                ));
            }
        }
        found.insert(stem, src);
    }
    found
}

#[test]
fn every_widget_owns_its_complete_folder() {
    let dir = root().join("widgets/src/widgets");
    let manifest =
        std::fs::read_to_string(root().join("widgets/Cargo.toml")).expect("widget manifest");
    let mut wrong = Vec::new();
    for name in widgets().keys() {
        let folder = dir.join(name);
        for required in ["main.rs", "help.txt", "README.md", "CONFIGURE.md"] {
            if !folder.join(required).is_file() {
                wrong.push(format!("{name}: missing {required}"));
            }
        }
        let path = format!("path = \"src/widgets/{name}/main.rs\"");
        if !manifest.contains(&path) {
            wrong.push(format!("{name}: Cargo.toml does not point at its folder"));
        }
        let source = std::fs::read_to_string(folder.join("main.rs")).unwrap_or_default();
        if !source.contains("include_str!(\"CONFIGURE.md\")")
            || !source.contains("maybe_widget_help")
        {
            wrong.push(format!("{name}: binary does not carry its CONFIGURE.md"));
        }
        let settings = folder.join("settings.json");
        if settings.exists() {
            let text = std::fs::read_to_string(&settings).unwrap_or_default();
            match serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|value| value.as_object().cloned())
            {
                None => wrong.push(format!("{name}: settings.json is not a JSON object")),
                Some(settings) => {
                    for key in settings.keys().filter(|key| !key.starts_with('_')) {
                        let has_help = [
                            format!("_{key}_comment"),
                            format!("_comment_{key}"),
                            format!("_{key}"),
                        ]
                        .iter()
                        .any(|comment| {
                            settings
                                .get(comment)
                                .and_then(serde_json::Value::as_str)
                                .is_some_and(|text| !text.trim().is_empty())
                        });
                        if !has_help {
                            wrong.push(format!(
                                "{name}: {key} has no field-specific help in settings.json"
                            ));
                        }
                    }
                }
            }
            if !source.contains("tc::run_settings") {
                wrong.push(format!("{name}: declares settings but never opens them"));
            }
            let readme = std::fs::read_to_string(folder.join("README.md")).unwrap_or_default();
            if !readme.contains("`,`") {
                wrong.push(format!("{name}: README does not document the settings key"));
            }
        } else if source.contains("tc::run_settings") {
            wrong.push(format!("{name}: opens a settings screen with no declaration"));
        }
    }
    let launcher = root().join("widgets/src/launcher");
    for required in [
        "main.rs",
        "help.txt",
        "README.md",
        "CONFIGURE.md",
        "settings.json",
    ] {
        if !launcher.join(required).is_file() {
            wrong.push(format!("launcher: missing {required}"));
        }
    }
    let launcher_source =
        std::fs::read_to_string(launcher.join("main.rs")).unwrap_or_default();
    if !launcher_source.contains("tc::run_settings") {
        wrong.push("launcher: does not expose shared terminal settings".into());
    }
    let core = std::fs::read_to_string(root().join("core/src/lib.rs")).unwrap_or_default();
    let terminal =
        std::fs::read_to_string(launcher.join("settings.json")).unwrap_or_default();
    let terminal: serde_json::Value =
        serde_json::from_str(&terminal).expect("launcher settings are valid JSON");
    for key in terminal
        .as_object()
        .into_iter()
        .flatten()
        .map(|(key, _)| key)
        .filter(|key| !key.starts_with('_'))
    {
        if !core.contains(&format!("\"{}\"", key)) {
            wrong.push(format!("launcher: terminal.{key} is never read by core"));
        }
    }
    assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
}

#[test]
fn only_core_draws_the_settings_screen() {
    // The division this whole feature rests on: a widget declares what it
    // has to configure, and core decides what that looks like and how it
    // behaves. Fifteen widgets each with their own idea of what enter does
    // to a boolean is the thing being prevented, and it is the kind of drift
    // that arrives one reasonable-looking exception at a time.
    //
    // Convention held it until now. Convention is what the contrast rule was
    // for as long as there were four widgets to break it.
    let mut wrong = Vec::new();
    for (name, src) in widgets() {
        let dir = root().join("widgets/src/widgets").join(&name);
        let declares = dir.join("settings.json").exists();

        // Ships settings data, so it must hand them to the shared screen.
        // A widget with a settings.json and no run_settings either has no
        // way in, or has built one.
        if declares {
            if !src.contains("SettingsSpec") {
                wrong.push(format!("{name}: has settings.json but declares no SettingsSpec"));
            }
            if !src.contains("run_settings") {
                wrong.push(format!("{name}: has settings.json but never calls tc::run_settings"));
            }
        }

        // And must not have grown a screen of its own. Named shapes only -
        // a function or module that says settings in its name - because a
        // check that guessed from the drawing calls would flag every widget
        // that draws anything.
        for line in src.lines() {
            let line = line.trim();
            if line.starts_with("//") {
                continue;
            }
            let own_module = line.starts_with("mod settings") || line.starts_with("pub mod settings");
            let own_fn = line
                .split_once("fn ")
                .map(|(before, after)| {
                    !before.ends_with('.')
                        && after
                            .split(['(', '<', ' '])
                            .next()
                            .is_some_and(|n| n.contains("settings"))
                })
                .unwrap_or(false);
            if own_module || own_fn {
                wrong.push(format!(
                    "{name}: defines its own settings code - {}",
                    line.chars().take(60).collect::<String>()
                ));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "settings belong to core, not to a widget:\n  {}\n\
         A widget declares its settings in settings.json - defaults, a _schema \
         of rules, and a _<key>_comment for each - and calls tc::run_settings. \
         Anything the screen cannot already do is a change to core/src/settings.rs \
         so every widget gets it, not a screen of this one's own.",
        wrong.join("\n  ")
    );
}

/// The header the generated file carries, naming what regenerates it.
const CONFIG_EXAMPLE_COMMENT: &str = "Generated from each widget's \
settings.json by widgets/tests/check.rs - rewrite it with \
`UPDATE_CONFIG_EXAMPLE=1 cargo test --test check generated_config_example_matches_widget_settings`. \
Copy to config.json (git-ignored) or ~/.config/opscope/config.json. Every \
key is optional; anything omitted keeps the widget's default.";

/// A JSON value that remembers the order its keys were written in.
///
/// `serde_json::Value` does not: its map is a `BTreeMap` unless the
/// `preserve_order` feature is on, and that feature is not a local decision.
/// Cargo unifies features across the graph, so switching it on for this test
/// would switch it on for every widget - and under `cargo test` the widgets
/// would iterate their config in file order while the release build kept
/// sorting it. A divergence between what is tested and what ships is worse
/// than either ordering.
///
/// Order is load-bearing in the generated file. Each `_<key>_comment` sits
/// directly above the key it describes, and sorting scatters them: `ports`
/// would read `_comment`, `_refresh_comment`, `_system_ports_comment`,
/// `refresh`, `system_ports`, with every explanation two rows from its key.
enum Ordered {
    Object(Vec<(String, Ordered)>),
    Array(Vec<Ordered>),
    /// Anything with no order to lose - `serde_json` renders these, so the
    /// escaping and the number formatting are its and not this file's.
    Leaf(serde_json::Value),
}

impl<'de> serde::Deserialize<'de> for Ordered {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Any;
        impl<'de> serde::de::Visitor<'de> for Any {
            type Value = Ordered;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("any JSON value")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Ordered, A::Error> {
                let mut pairs = Vec::new();
                while let Some(pair) = map.next_entry::<String, Ordered>()? {
                    pairs.push(pair);
                }
                Ok(Ordered::Object(pairs))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Ordered, A::Error> {
                let mut items = Vec::new();
                while let Some(item) = seq.next_element::<Ordered>()? {
                    items.push(item);
                }
                Ok(Ordered::Array(items))
            }
            fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Ordered, E> {
                Ok(Ordered::Leaf(v.into()))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Ordered, E> {
                Ok(Ordered::Leaf(v.into()))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Ordered, E> {
                Ok(Ordered::Leaf(v.into()))
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Ordered, E> {
                Ok(Ordered::Leaf(v.into()))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Ordered, E> {
                Ok(Ordered::Leaf(v.into()))
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Ordered, E> {
                Ok(Ordered::Leaf(serde_json::Value::Null))
            }
            fn visit_none<E: serde::de::Error>(self) -> Result<Ordered, E> {
                Ok(Ordered::Leaf(serde_json::Value::Null))
            }
        }
        d.deserialize_any(Any)
    }
}

impl serde::Serialize for Ordered {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::{SerializeMap, SerializeSeq};
        match self {
            Ordered::Object(pairs) => {
                let mut map = s.serialize_map(Some(pairs.len()))?;
                for (key, value) in pairs {
                    map.serialize_entry(key, value)?;
                }
                map.end()
            }
            Ordered::Array(items) => {
                let mut seq = s.serialize_seq(Some(items.len()))?;
                for item in items {
                    seq.serialize_element(item)?;
                }
                seq.end()
            }
            Ordered::Leaf(value) => value.serialize(s),
        }
    }
}

fn read_ordered(path: &std::path::Path) -> Ordered {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// A section as the public example carries it: everything but the rules.
///
/// `_schema` is the widget telling the settings screen what a value may be -
/// a minimum, what an array holds, which choices a field offers. None of it
/// is a setting, so none of it belongs in a file people copy to
/// `config.json`. Dropped at the top of a section only, which is where it
/// lives; a `_schema` nested inside a value would be somebody's own key.
fn without_schema(value: Ordered) -> Ordered {
    match value {
        Ordered::Object(pairs) => {
            Ordered::Object(pairs.into_iter().filter(|(k, _)| k != "_schema").collect())
        }
        other => other,
    }
}

/// `config.example.json`, built from the settings each widget owns.
///
/// This was `tools/config-example.py` until the port, and the check that
/// read it ran
/// `python3` as a subprocess - so `cargo test` failed on a machine without
/// python3, for a reason it was not testing. A test that can fail for a
/// reason it does not test teaches people to press the button again until it
/// goes green, which is how a real failure gets waved past.
fn render_config_example() -> String {
    let root = root();
    let mut top = vec![(
        "_comment".to_string(),
        Ordered::Leaf(serde_json::Value::String(CONFIG_EXAMPLE_COMMENT.to_string())),
    )];
    // Shared terminal settings first, out of alphabetical position on
    // purpose: they are not a widget's and they belong at the top.
    top.push((
        "terminal".to_string(),
        without_schema(read_ordered(
            &root.join("widgets/src/launcher/settings.json"),
        )),
    ));
    // Sorted by folder, which is the order the file has always been in:
    // `github`, `github-actions`, `github-prs` sort by the hyphenated name
    // rather than by the underscored section it becomes.
    let dir = root.join("widgets/src/widgets");
    let mut folders: Vec<String> = std::fs::read_dir(&dir)
        .expect("the widget directory")
        .flatten()
        .filter(|entry| entry.path().join("settings.json").exists())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    folders.sort();
    assert!(!folders.is_empty(), "no widget declares any settings");
    for folder in folders {
        top.push((
            folder.replace('-', "_"),
            without_schema(read_ordered(&dir.join(&folder).join("settings.json"))),
        ));
    }
    let mut out = serde_json::to_string_pretty(&Ordered::Object(top))
        .expect("the config example serialises");
    out.push('\n');
    out
}

#[test]
fn generated_config_example_matches_widget_settings() {
    let path = root().join("config.example.json");
    let generated = render_config_example();
    // Writing first and comparing after means a regenerating run still
    // proves the result, rather than rewriting the file and reporting
    // nothing about it.
    if std::env::var_os("UPDATE_CONFIG_EXAMPLE").is_some() {
        std::fs::write(&path, &generated).expect("rewriting config.example.json");
    }
    let current = std::fs::read_to_string(&path).unwrap_or_default();
    if current == generated {
        return;
    }
    // The whole file is 250 lines; printing both is not a diff anybody
    // reads. Say which line parted company.
    let parted = current
        .lines()
        .zip(generated.lines())
        .position(|(a, b)| a != b);
    let detail = match parted {
        Some(n) => format!(
            "line {} differs:\n     on disk: {}\n   generated: {}",
            n + 1,
            current.lines().nth(n).unwrap_or(""),
            generated.lines().nth(n).unwrap_or("")
        ),
        None => format!(
            "the shorter file is a prefix of the other: {} lines on disk, {} generated",
            current.lines().count(),
            generated.lines().count()
        ),
    };
    panic!(
        "config.example.json is not what the widget settings generate.\n  {detail}\n\n\
         Rewrite it with:\n  UPDATE_CONFIG_EXAMPLE=1 cargo test --test check \
         generated_config_example_matches_widget_settings -- --exact"
    );
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
        let doc = root()
            .join("widgets/src/widgets")
            .join(&name)
            .join("README.md");
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
                wrong.push(format!("{}: [{}] in the footer, not in its README.md", name, key));
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
        let doc = root()
            .join("widgets/src/widgets")
            .join(name)
            .join("README.md");
        assert!(doc.exists(), "{} has no owned README.md", name);
        let cell = format!(
            "[`{}`](../widgets/src/widgets/{}/README.md)",
            name, name
        );
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
    let text = std::fs::read_to_string(root().join("widgets/src/launcher/README.md"))
        .expect("launcher README.md");
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
                "the launcher README lists {:?} in the sample and no widget is called that",
                stem
            ));
        }
    }
    assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
}

/// Every widget is on the launcher's menu, and every name on it is a widget.
///
/// `WIDGETS` in the launcher is the keystone of a new widget and was enforced
/// by nothing. Half of it looks enforced: the `widget!` macro `include_str!`s
/// the named folder's `help.txt` and `README.md`, so a *wrong* name there is
/// a compile error, and that is the half people notice.
///
/// Omission is the half the compiler cannot see. Add a widget folder, forget
/// the line, and the workspace builds, every other check passes, and the
/// widget simply never appears in `opscope` - which looks from the menu
/// exactly like a widget nobody wrote.
#[test]
fn every_widget_is_on_the_launcher_menu() {
    let source = std::fs::read_to_string(root().join("widgets/src/launcher/main.rs"))
        .expect("the launcher's main.rs");
    // Only the list. The `widget!` macro is defined in the same file, just
    // above it, and its definition names no widget.
    let list = source
        .split("const WIDGETS: &[Widget] = &[")
        .nth(1)
        .expect("the WIDGETS list")
        .split("];")
        .next()
        .expect("the end of the WIDGETS list");
    let listed: BTreeSet<String> = list
        .match_indices("widget!(\"")
        .filter_map(|(at, _)| {
            let after = &list[at + "widget!(\"".len()..];
            after.find('"').map(|end| after[..end].to_string())
        })
        .collect();
    // A pattern that matches nothing reads exactly like a menu with nothing
    // on it, and this file has been fooled that way before.
    assert!(
        !listed.is_empty(),
        "no widget!(\"...\") entries came out of the launcher - that is this \
         pattern being wrong, not the menu being empty"
    );

    let built: BTreeSet<String> = widgets().into_keys().collect();
    let mut wrong = Vec::new();
    for name in built.difference(&listed) {
        wrong.push(format!(
            "{name}: is a widget and is not in the launcher's WIDGETS, so it \
             never appears in the opscope menu"
        ));
    }
    for name in listed.difference(&built) {
        wrong.push(format!(
            "{name}: is in the launcher's WIDGETS and is not a widget"
        ));
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
    let dir = root().join("widgets/src/widgets");
    let mut wrong = Vec::new();
    for (name, src) in widgets() {
        let help = dir.join(&name).join("help.txt");
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
                        "{}/help.txt names {:?} and its main.rs does not answer it",
                        name, word
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
        let at = src.find(from).unwrap_or_else(|| panic!("{} moved or was renamed", what));
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


/// A catalogue is wired to a field by name, and nothing else ties them.
///
/// `catalogues: &[("rates", LIST_RATES)]` says "offer this table on the field
/// called rates". Write `"rate"` and it compiles, every test passes, and the
/// settings screen quietly goes on showing a JSON box - the exact failure
/// this file exists for, since the only symptom is a feature that is simply
/// not there.
#[test]
fn every_catalogue_names_a_field_its_widget_actually_declares() {
    let dir = root().join("widgets/src/widgets");
    let mut wrong = Vec::new();
    for name in widgets().keys() {
        let source = std::fs::read_to_string(dir.join(name).join("main.rs")).unwrap_or_default();
        let Some(rest) = source.split("catalogues: &[").nth(1) else {
            continue;
        };
        let Some(list) = rest.split("],").next() else {
            continue;
        };
        // Each entry opens ("field-name", TABLE.
        let declared: Vec<String> = list
            .match_indices("(\"")
            .filter_map(|(at, _)| {
                let after = &list[at + 2..];
                after.find('"').map(|end| after[..end].to_string())
            })
            .collect();
        if declared.is_empty() {
            continue;
        }
        let settings =
            std::fs::read_to_string(dir.join(name).join("settings.json")).unwrap_or_default();
        let parsed: serde_json::Value = match serde_json::from_str(&settings) {
            Ok(v) => v,
            Err(_) => {
                wrong.push(format!("{name}: declares a catalogue and has no settings.json"));
                continue;
            }
        };
        for field in declared {
            if !parsed.as_object().is_some_and(|o| o.contains_key(&field)) {
                wrong.push(format!(
                    "{name}: catalogue names {field:?}, which is not a key in its settings.json"
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
}


/// The `token_env` default on screen is the one the code falls back to.
///
/// The settings screen draws a field's default from `settings.json`, and the
/// widget reads its own fallback from `TOKEN_ENV`. Nothing tied the two
/// together, and they had already come apart: `github-actions` declared `""`
/// while its code fell back to `GITHUB_TOKEN`, so the screen showed an empty
/// default for a variable that was in fact being read. Somebody reading that
/// screen would set a variable nothing looks at, or none at all.
#[test]
fn a_declared_token_env_matches_the_code() {
    let dir = root().join("widgets/src/widgets");
    let mut wrong = Vec::new();
    for name in widgets().keys() {
        let source =
            std::fs::read_to_string(dir.join(name).join("main.rs")).unwrap_or_default();
        // `const TOKEN_ENV: &str = "SOMETHING";`
        let Some(at) = source.find("const TOKEN_ENV: &str = \"") else {
            continue;
        };
        let rest = &source[at + "const TOKEN_ENV: &str = \"".len()..];
        let Some(end) = rest.find('"') else {
            continue;
        };
        let in_code = &rest[..end];

        let settings =
            std::fs::read_to_string(dir.join(name).join("settings.json")).unwrap_or_default();
        let parsed: serde_json::Value = match serde_json::from_str(&settings) {
            Ok(v) => v,
            Err(_) => {
                wrong.push(format!("{name}: names a TOKEN_ENV and has no settings.json"));
                continue;
            }
        };
        let declared = parsed.get("token_env").and_then(|v| v.as_str());
        match declared {
            Some(shown) if shown == in_code => {}
            Some(shown) => wrong.push(format!(
                "{name}: settings.json shows token_env {shown:?}, the code uses {in_code:?}"
            )),
            None => wrong.push(format!(
                "{name}: names a TOKEN_ENV of {in_code:?} and declares no token_env default"
            )),
        }
    }
    assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
}


/// The default on screen is the default the widget uses.
///
/// The settings screen draws a field's default from `settings.json`; the
/// widget falls back to whatever it passed `cfg_*`. Nothing held the two
/// together, and they had come apart: `latency` shipped
/// `["1.1.1.1", "8.8.8.8", "example.internal"]` in its schema while its code
/// defaulted to the first two, so the screen advertised a third host that
/// was never pinged - and writing anything materialised the screen's version
/// into the file, at which point the widget started pinging a name that does
/// not resolve.
///
/// Only the four `cfg_*` helpers with a literal default are read. A default
/// that is computed cannot be compared against a constant, and is skipped
/// rather than guessed at.
#[test]
fn a_declared_default_matches_the_code() {
    let dir = root().join("widgets/src/widgets");
    let mut wrong = Vec::new();
    for (name, source) in widgets() {
        let settings = std::fs::read_to_string(dir.join(&name).join("settings.json"))
            .unwrap_or_default();
        let Ok(schema) = serde_json::from_str::<serde_json::Value>(&settings) else {
            continue;
        };
        let Some(schema) = schema.as_object() else {
            continue;
        };
        // Whitespace flattened first: these calls wrap across lines, and a
        // line-by-line read is how a config audit here once missed most of
        // them.
        let flat: String = source.split_whitespace().collect::<Vec<_>>().join(" ");

        for (call, render) in [
            ("cfg_f64(", "number"),
            ("cfg_usize(", "number"),
            ("cfg_str(", "string"),
            ("cfg_strings(", "strings"),
        ] {
            let mut rest = flat.as_str();
            while let Some(at) = rest.find(call) {
                rest = &rest[at + call.len()..];
                let Some(end) = rest.find(')') else { break };
                let args = &rest[..end];
                // cfg, "key", default
                let mut parts = args.splitn(3, ',');
                let _cfg = parts.next();
                let Some(key) = parts.next() else { continue };
                let Some(default) = parts.next() else { continue };
                let key = key.trim().trim_matches('"');
                let default = default.trim();
                let Some(declared) = schema.get(key) else {
                    continue;
                };
                // Only a literal can be compared against a constant. A named
                // one - TOKEN_ENV and its like - is held to its schema by
                // a_declared_token_env_matches_the_code instead, which knows
                // how to resolve it.
                let literal = default.starts_with('"')
                    || default.starts_with("&[")
                    || default.starts_with('-')
                    || default.starts_with(|c: char| c.is_ascii_digit());
                if !literal {
                    continue;
                }
                let agrees = match render {
                    "number" => default
                        .parse::<f64>()
                        .ok()
                        .zip(declared.as_f64())
                        .is_some_and(|(a, b)| (a - b).abs() < 1e-9),
                    "string" => declared.as_str() == Some(default.trim_matches('"')),
                    // &["a", "b"] against the declared array.
                    "strings" => {
                        let inner = default.trim_start_matches("&[").trim_end_matches(']');
                        let listed: Vec<String> = inner
                            .split(',')
                            .map(|s| s.trim().trim_matches('"').to_string())
                            .filter(|s| !s.is_empty())
                            .collect();
                        declared
                            .as_array()
                            .map(|a| {
                                a.iter()
                                    .filter_map(|v| v.as_str())
                                    .map(str::to_string)
                                    .collect::<Vec<_>>()
                            })
                            .is_some_and(|d| d == listed)
                    }
                    _ => true,
                };
                if !agrees {
                    wrong.push(format!(
                        "{name}.{key}: settings.json declares {}, the code falls back to {}",
                        serde_json::to_string(declared).unwrap_or_default(),
                        default
                    ));
                }
            }
        }
    }
    wrong.sort();
    wrong.dedup();
    assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
}


/// Every array field declares what it holds.
///
/// The settings screen decides whether a list is filled in entry by entry or
/// left as a JSON box, and it decides from `items`. Where that is missing it
/// falls back to reading the shipped default - which works until the default
/// is empty, and a list somebody is meant to fill in ships empty by nature.
///
/// This is not hypothetical. `latency.strip_suffixes` was classified by its
/// default holding one string; correcting that default to `[]` - a separate,
/// correct fix - silently took its editor away, and every test still passed
/// because nothing checked that a field kept the screen it had. A capability
/// that depends on a value is a capability that leaves when the value does.
#[test]
fn every_array_declares_what_it_holds() {
    let dir = root().join("widgets/src/widgets");
    let mut wrong = Vec::new();
    for name in widgets().keys() {
        let settings =
            std::fs::read_to_string(dir.join(name).join("settings.json")).unwrap_or_default();
        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&settings) else {
            continue;
        };
        let Some(body) = parsed.as_object() else {
            continue;
        };
        let schema = body.get("_schema").and_then(|v| v.as_object());
        for (key, value) in body {
            if key.starts_with('_') || !value.is_array() {
                continue;
            }
            let rule = schema.and_then(|s| s.get(key)).and_then(|r| r.as_object());
            // A picker names its own answers, so it has already said.
            if rule.is_some_and(|r| r.contains_key("picker")) {
                continue;
            }
            if !rule.is_some_and(|r| r.contains_key("items")) {
                wrong.push(format!(
                    "{name}.{key}: an array with no `items` - the screen would have to \
                     guess from its default what kind of list it is"
                ));
            }
        }
    }
    wrong.sort();
    assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
}
