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

//! The command palette: `ctrl-k` in any widget.
//!
//! One box over the middle of the pane - a filter line and a list under
//! it - and every mode of it is the same box. `ctrl-k` opens the
//! commands; the Theme action swaps the box's contents to the theme
//! list (the picker, unchanged), and Switch widget to the other
//! widgets. esc pops one level: the theme list back to the commands,
//! the commands back to the widget. A mode never draws over another;
//! each redraws over the frame the palette was opened on, so the modes
//! read as one surface that changes, not a stack of boxes that grows.
//!
//! The theme picker also keeps its own `ctrl-t`, asked of the widget
//! directly: the same picker, over the widget's frame, with esc back to
//! the widget rather than to the palette.
//!
//! The commands are the widget-wide actions core owns, so every widget
//! gets all of them with no widget code: the theme picker, this
//! widget's settings (a widget registers its settings spec at startup
//! with [`settings::register_settings`]), notices, and switching this
//! pane to another widget. A command that already has a key shows it - `,`
//! settings, `!` notices - so the palette teaches the footer's keys
//! while it is the route around them.
//!
//! The widgets to switch to come from the build manifest, the same file
//! the launcher's own list is checked against, rather than a second copy
//! here that can drift.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::picker::{self, Modal};
use crate::settings;
use crate::theme::{self, Role};

/// Open, so the key that opens it is not answered again inside it, and so
/// a theme switched in another pane waits for the palette to close
/// rather than relaunching the widget under it.
pub(crate) static OPEN: AtomicBool = AtomicBool::new(false);

/// The key that opens it, and the hint every footer carries for it.
pub const KEY: &str = "ctrl-k";
/// In the footer's own form, `[q]uit` - the key in brackets, fused onto
/// the word - and a click on it is `ctrl-k`.
///
/// Twelve cells on purpose, the width `[,] settings` sits at and the
/// old theme-picker hint sat at: `pack_hints` will not split a hint, so
/// a wider one overflows a twelve-column pane, and twelve is the floor
/// every frame in the tree is measured from. "cmds" because that is
/// what the press opens, in the shortest form that fits.
pub const HINT: &str = "[ctrl-k]cmds";

/// The box's last row: each key as a chip, and what it does. A click on
/// a chip or its word is that key.
const KEYS: &[(&str, &str)] = &[("↑↓", "choose"), ("↵", "run"), ("esc", "back")];

/// Where each key's chip and word sit on the keys row, in cells from the
/// row's start: the key, and the cells from its chip to the end of its
/// word. One layout for drawing the row and for reading a click on it.
///
/// Where the row with its words would not fit `iw`, it is the chips alone:
/// a box that narrow still says which keys work, rather than cutting a
/// word in half.
fn key_spans(iw: usize) -> Vec<(&'static str, usize, usize)> {
    let lay = |words: bool| {
        let mut out = Vec::new();
        let mut at = 1;
        for (key, word) in KEYS {
            let start = at;
            at += crate::display_width(key) + 2;
            if words {
                at += 1 + crate::display_width(word);
            }
            out.push((*key, start, at));
            at += if words { 3 } else { 1 };
        }
        out
    };
    let full = lay(true);
    if full.last().is_some_and(|(_, _, end)| *end <= iw) { full } else { lay(false) }
}

/// Whether the keys row has room for its words at `iw`.
fn keys_with_words(iw: usize) -> bool {
    let spans = key_spans(iw);
    spans.len() < 2 || spans[1].1 - spans[0].2 > 1
}

/// Rows the box spends around its list: the title band, the filter line,
/// the rule under it, and the keys.
const AROUND: usize = 4;

/// How many rows of the list the box shows: one option a row, as the
/// theme list has them, so every mode of the box reads with the same line
/// height.
pub fn slots(ih: usize) -> usize {
    ih.saturating_sub(AROUND).max(1)
}

/// One row of whichever list the box is showing.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub label: String,
    pub detail: String,
    /// The key that does the same thing without the palette, shown on
    /// the row so the palette teaches it. `None` when there is no such
    /// key.
    pub key: Option<&'static str>,
    pub runs: Runs,
    /// For [`Runs::Send`]: the key handed to the widget.
    pub sends: Option<String>,
}

/// What ↵ does on a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Runs {
    /// The theme picker, in this same box: [`picker::run_over`].
    Theme,
    /// This widget's settings screen, registered at startup.
    Settings,
    /// The notices screen.
    Notices,
    /// The other widgets, in this same box.
    Widgets,
    /// Switch this pane to the widget named by the row: this one ends and
    /// that one takes its place.
    Widget,
    /// One of this widget's own footer actions: close the palette and hand
    /// the widget the key, as if it were typed.
    Send,
    /// This widget's `--help` text, in this same box.
    Help,
    /// A line of that text: nothing to run.
    Line,
    /// Start this widget again with its charts drawn the other way, text
    /// or pixels.
    Graphics,
    /// Start this widget again, as saving its settings does.
    Restart,
    /// Put what identifies this build and terminal on the clipboard.
    CopyDiagnostics,
    /// Put this widget's documentation link on the clipboard.
    CopyDocs,
    /// A section's heading: not a command, and the highlight steps over it.
    Header,
}

/// The keys core answers for every widget, which have rows of their own
/// here, so a footer hint naming one is not listed twice.
const CORE_KEYS: &[&str] = &[KEY, picker::KEY, ",", "!"];

/// Which way the chart switch can go from here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Charts {
    /// Charts are pixels, or pixels were asked for: the switch goes to text.
    ToText,
    /// Text was asked for: the switch goes to pixels.
    ToPixels,
    /// Nothing to switch: `NO_COLOR` keeps them text, or the terminal draws
    /// no images and nobody asked for any.
    Neither,
}

impl Charts {
    /// The switch for this run. Pixels asked for count as pixels even where
    /// the terminal drew none, so there is always a way back to text from a
    /// switch that did not take.
    pub fn now() -> Charts {
        if theme::no_color() {
            Charts::Neither
        } else if crate::terminal::text_requested() {
            Charts::ToPixels
        } else if crate::capabilities().kitty_graphics || crate::terminal::pixels_requested() {
            Charts::ToText
        } else {
            Charts::Neither
        }
    }
}

/// The commands on offer: core's own, then this widget's footer actions
/// (`actions`, each its key and the text its footer drew).
///
/// Settings only when this binary registered a settings spec - the
/// launcher and `matrix` do not, and a Settings command that answered with
/// nothing would be a hint bound to nothing. The chart switch only where
/// it has somewhere to go ([`Charts`]).
pub fn commands(actions: &[(String, String)], charts: Charts) -> Vec<Row> {
    let row = |label: &str, detail: &str, key: Option<&'static str>, runs: Runs| Row {
        label: label.into(),
        detail: detail.into(),
        key,
        runs,
        sends: None,
    };
    let mut rows = vec![row("Theme…", "pick a theme for this pane", Some("ctrl-t"), Runs::Theme)];
    if settings::registered().is_some() {
        rows.push(row("Settings…", "this widget's settings", Some(","), Runs::Settings));
    }
    rows.push(row("Switch widget…", "replace this widget with another", None, Runs::Widgets));
    rows.push(row("Notices", "warnings this widget raised", Some("!"), Runs::Notices));
    rows.push(row("Keys & help…", "every key this widget answers", None, Runs::Help));
    match charts {
        Charts::ToText => rows.push(row("Charts as text", "restart with text charts: less to send over SSH", None, Runs::Graphics)),
        Charts::ToPixels => rows.push(row("Charts as pixels", "restart with image charts, where the terminal can", None, Runs::Graphics)),
        Charts::Neither => {}
    }
    rows.push(row("Restart widget", "start again, as saving settings does", None, Runs::Restart));
    rows.push(row("Copy diagnostics", "version, theme, terminal: for a bug report", None, Runs::CopyDiagnostics));
    rows.push(row("Copy docs link", "this widget's docs, to the clipboard", None, Runs::CopyDocs));
    let mut seen: Vec<&str> = Vec::new();
    for (key, text) in actions {
        if CORE_KEYS.contains(&key.as_str()) || seen.contains(&key.as_str()) {
            continue;
        }
        seen.push(key);
        rows.push(Row {
            label: text.clone(),
            detail: "this widget".into(),
            key: None,
            runs: Runs::Send,
            sends: Some(key.clone()),
        });
    }
    rows
}

impl Row {
    /// What the recent list remembers a row by: a widget action by its key,
    /// since its text can change with the state it toggles (`[p]omodoro`
    /// becomes `[p]omodoro off`), and anything else by its label.
    pub fn id(&self) -> String {
        match &self.sends {
            Some(key) => format!("key:{key}"),
            None => self.label.clone(),
        }
    }
}

/// `rows` with the ones in `recent` first, most recent first, and the rest
/// in their own order: the commands somebody reaches for sit at the top.
pub fn by_recent(rows: Vec<Row>, recent: &[String]) -> Vec<Row> {
    let mut out: Vec<Row> = recent
        .iter()
        .filter_map(|id| rows.iter().find(|r| &r.id() == id).cloned())
        .collect();
    out.extend(rows.into_iter().filter(|r| !recent.contains(&r.id())));
    out
}

/// How many commands are remembered as recent.
const RECENT: usize = 5;

/// The file the recent commands are kept in, shared by every widget: a
/// command is a command whichever pane it was run in.
fn recent_path() -> std::path::PathBuf {
    crate::statefile::state_dir().join("palette-recent.json")
}

/// The recent commands, most recent first. Nothing when there is no file
/// or it cannot be read: the order is a convenience, never a failure.
pub fn recent() -> Vec<String> {
    std::fs::read_to_string(recent_path())
        .ok()
        .and_then(|t| serde_json::from_str::<Vec<String>>(&t).ok())
        .unwrap_or_default()
}

/// `recent` with `id` moved to the front, kept to [`RECENT`].
pub fn remembered(mut recent: Vec<String>, id: &str) -> Vec<String> {
    recent.retain(|l| l != id);
    recent.insert(0, id.to_string());
    recent.truncate(RECENT);
    recent
}

/// Note the row `id` as just run. A failed write is ignored for the same
/// reason a failed read is.
fn remember(id: &str) {
    let next = remembered(recent(), id);
    if let Ok(text) = serde_json::to_string(&next) {
        let path = recent_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = crate::statefile::write_atomic(&path, &text);
    }
}

/// The help text's lines as written, with prose rejoined: `help.txt` is
/// already wrapped for an 80-column terminal, and wrapping those lines
/// again for a narrower box leaves a word or two stranded on a row of its
/// own. A line that starts at the margin continues the one before it when
/// that one did too; an indented line - a key table, a list - keeps its
/// own line, since its breaks are the layout.
fn paragraphs(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut joining = false;
    for raw in text.lines().map(str::trim_end) {
        if raw.trim().is_empty() {
            joining = false;
            continue;
        }
        let prose = !raw.starts_with(char::is_whitespace);
        match out.last_mut() {
            Some(last) if joining && prose => {
                last.push(' ');
                last.push_str(raw.trim());
            }
            _ => out.push(raw.to_string()),
        }
        joining = prose;
    }
    out
}

/// The help text as rows to read and filter, wrapped to `width` cells -
/// never cut, which would teach half a sentence - with a line's own indent
/// kept on the lines it wraps onto. Blank lines are dropped, since a
/// filtered list has no paragraphs to keep apart.
pub fn help_rows(text: &str, width: usize) -> Vec<Row> {
    let width = width.max(8);
    let mut out = Vec::new();
    for raw in paragraphs(text).iter().map(String::as_str) {
        // A line that fits is kept as written: its spacing is the
        // alignment of a key table.
        if crate::display_width(raw) <= width {
            out.push(raw.to_string());
            continue;
        }
        let indent = raw.len() - raw.trim_start().len();
        let lead = " ".repeat(indent.min(width / 2));
        let mut line = lead.clone();
        for word in raw.split_whitespace() {
            let room = width.saturating_sub(crate::display_width(&line));
            let need = crate::display_width(word) + usize::from(line.trim().len() > 0);
            if need > room && !line.trim().is_empty() {
                out.push(line);
                line = lead.clone();
            }
            if !line.trim().is_empty() {
                line.push(' ');
            }
            // A single word wider than the box is broken by hand: the
            // one case where there is nowhere else for it to go.
            let mut word = word.to_string();
            while crate::display_width(&line) + crate::display_width(&word) > width {
                let fit = width.saturating_sub(crate::display_width(&line)).max(1);
                let head: String = word.chars().take(fit).collect();
                word = word.chars().skip(fit).collect();
                line.push_str(&head);
                out.push(line);
                line = lead.clone();
            }
            line.push_str(&word);
        }
        if !line.trim().is_empty() {
            out.push(line);
        }
    }
    out.into_iter()
        .map(|l| Row {
            label: l,
            detail: String::new(),
            key: None,
            runs: Runs::Line,
            sends: None,
        })
        .collect()
}

/// What identifies this build, theme, terminal and multiplexer, as text for
/// a bug report. Nothing from the config or the file system: no path, no
/// host, nothing that would need taking out before it is pasted.
pub fn diagnostics(var: impl Fn(&str) -> Option<String>, w: usize, h: usize) -> String {
    let t = theme::current();
    let set = |k: &str| var(k).is_some_and(|v| !v.is_empty());
    let mux = if set("HERDR_ENV") {
        "Herdr"
    } else if set("LUVUS_ENV") {
        "Luvus"
    } else if set("REX_SESSION") || set("REX_BLOCK") {
        "Rex"
    } else if set("TMUX") {
        "tmux"
    } else {
        "none"
    };
    let charts = if crate::capabilities().kitty_graphics && !crate::terminal::text_requested() {
        "pixels"
    } else {
        "text"
    };
    let or = |k: &str| var(k).filter(|v| !v.is_empty()).unwrap_or_else(|| "-".into());
    format!(
        "{}\ntheme: {} ({})\ncharts: {charts}\nterminal: TERM={} TERM_PROGRAM={} {}\nmultiplexer: {mux}\npane: {w}x{h}\nos: {} {}",
        crate::version(),
        t.id,
        t.name,
        or("TERM"),
        or("TERM_PROGRAM"),
        or("TERM_PROGRAM_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
    )
}

/// Where this widget's documentation is: its folder in the repository,
/// where its README sits, or the repository's own README for the launcher
/// and anything the manifest does not name.
pub fn docs_link(stem: &str) -> String {
    const REPO: &str = "https://github.com/stealth-factory/opscope";
    if stem != "opscope" && built().iter().any(|b| b == stem) {
        format!("{REPO}/tree/main/widgets/src/widgets/{stem}")
    } else {
        format!("{REPO}#readme")
    }
}

/// The name this binary runs under, without a path.
fn this_stem() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.file_stem().map(|stem| stem.to_string_lossy().into_owned()))
        .unwrap_or_default()
}

/// The binaries the manifest builds. Names come from there rather than
/// a list here: the manifest is the thing that knows what was built,
/// and the launcher's own test checks its list against this same file.
const MANIFEST: &str = include_str!("../../widgets/Cargo.toml");

/// Every binary the manifest names, alphabetical, as the launcher lists
/// them - so a widget sits where it sits in the menu, not somewhere
/// else because a manifest section moved.
pub fn built() -> Vec<String> {
    let mut names = Vec::new();
    let mut in_bin = false;
    for line in MANIFEST.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_bin = line == "[[bin]]";
            continue;
        }
        if in_bin {
            if let Some(rest) = line.strip_prefix("name = \"") {
                if let Some(name) = rest.strip_suffix('"') {
                    names.push(name.to_string());
                }
            }
        }
    }
    names.sort();
    names
}

/// The widgets Switch offers: every binary but the launcher, which is
/// this pane's own menu and not a widget to start, and the one already
/// running here.
pub fn others() -> Vec<String> {
    let here = this_stem();
    built()
        .into_iter()
        .filter(|name| name != "opscope" && *name != here)
        .collect()
}

/// Switch's rows: one widget per row, named by its binary.
pub fn switch_rows() -> Vec<Row> {
    others()
        .into_iter()
        .map(|stem| Row {
            label: stem,
            detail: String::new(),
            key: None,
            runs: Runs::Widget,
            sends: None,
        })
        .collect()
}

/// The rows that match `query`, best first; their own order when the
/// query is empty. The theme picker's matcher, so both modes of the one
/// box filter the same way.
pub fn matching(rows: &[Row], query: &str) -> Vec<Row> {
    let mut scored: Vec<(i32, usize, &Row)> = rows
        .iter()
        .enumerate()
        .filter_map(|(i, r)| {
            // Each field on its own, so a query never matches across
            // them; the label before the key and the detail words.
            let best = [
                picker::fuzzy(query, &r.label),
                r.key.and_then(|k| picker::fuzzy(query, k)),
                picker::fuzzy(query, &r.detail).map(|s| s + 20),
            ]
            .into_iter()
            .flatten()
            .min()?;
            Some((best, i, r))
        })
        .collect();
    scored.sort_by_key(|(score, i, _)| (*score, *i));
    scored.into_iter().map(|(_, _, r)| r.clone()).collect()
}

/// The commands in sections, for the list as it opens, before anything is
/// typed: what was run recently, then opscope's own, then this widget's
/// footer actions - each under a heading, and a row in one section only.
/// Typing drops the headings and ranks every command together, since a
/// match is a match whichever section it came from.
pub fn sectioned(rows: &[Row], recent: &[String]) -> Vec<Row> {
    let header = |label: &str| Row {
        label: label.into(),
        detail: String::new(),
        key: None,
        runs: Runs::Header,
        sends: None,
    };
    let is_recent = |r: &Row| recent.contains(&r.id());
    let recent_rows: Vec<Row> = recent
        .iter()
        .filter_map(|id| rows.iter().find(|r| &r.id() == id).cloned())
        .collect();
    let core: Vec<Row> = rows.iter().filter(|r| r.runs != Runs::Send && !is_recent(r)).cloned().collect();
    // Under its own heading, "this widget" on every row says it twice; it
    // is kept for the flat list a query ranks, where nothing else does.
    let own: Vec<Row> = rows
        .iter()
        .filter(|r| r.runs == Runs::Send && !is_recent(r))
        .map(|r| Row { detail: String::new(), ..r.clone() })
        .collect();
    let mut out = Vec::new();
    for (name, group) in [("RECENT", recent_rows), ("OPSCOPE", core), ("THIS WIDGET", own)] {
        if !group.is_empty() {
            out.push(header(name));
            out.extend(group);
        }
    }
    out
}

/// `at`, or the nearest row to it that is not a heading: ahead of it when
/// the highlight was moving down, behind it when moving up, and the other
/// way when there is nothing that way.
pub fn settle(list: &[Row], at: usize, forward: bool) -> usize {
    if list.is_empty() {
        return 0;
    }
    let at = at.min(list.len() - 1);
    let ok = |i: usize| list.get(i).is_some_and(|r| r.runs != Runs::Header);
    if ok(at) {
        return at;
    }
    let ahead = (at..list.len()).find(|&i| ok(i));
    let behind = (0..at).rev().find(|&i| ok(i));
    if forward { ahead.or(behind) } else { behind.or(ahead) }.unwrap_or(at)
}

/// Which characters of `label` the query matched, letter by letter in
/// order and ignoring case - the ones drawn picked out. Nothing when the
/// label does not hold every letter, as when the row matched on its
/// description instead.
pub fn matched(query: &str, label: &str) -> Vec<bool> {
    let mut want = query.chars().filter(|c| !c.is_whitespace()).flat_map(char::to_lowercase).peekable();
    let mut out: Vec<bool> = label
        .chars()
        .map(|c| {
            let hit = want.peek().is_some_and(|w| c.to_lowercase().next() == Some(*w));
            if hit {
                want.next();
            }
            hit
        })
        .collect();
    if want.peek().is_some() {
        out.iter_mut().for_each(|b| *b = false);
    }
    out
}

/// The rows a batch should be working with after a key changes the query
/// or the mode: what matches the query now, in the mode now.
///
/// `run` calls this after every key that edits the filter or switches
/// mode, rather than trusting the list the batch opened with - a paste
/// of `not` and ↵ arrives as one `poll`, and ↵ has to run the row the
/// `not` the reader typed is showing them.
pub fn refilter(commands: &[Row], switch: &[Row], help: &[Row], mode: Mode, query: &str, recent: &[String]) -> Vec<Row> {
    if mode == Mode::Commands && query.trim().is_empty() {
        return sectioned(commands, recent);
    }
    matching(
        match mode {
            Mode::Commands => commands,
            Mode::Widgets => switch,
            Mode::Help => help,
        },
        query,
    )
}

/// The box's inside, `iw` cells wide and `ih` rows tall, in the theme in
/// force:
///
/// ```text
///  ◆ Commands                                14 of 14     title band
///   ❯ ▏type to filter                                     filter
///  ──────────────────────────────────────────────────     rule
///   OPSCOPE ───────────────────────────────────────────   heading
/// ▌ Theme…  pick a theme for this pane          ctrl-t    chosen row
///   Settings…  this widget's settings                ,
///  ↑↓  choose   ↵  run   esc  back                        keys
/// ```
///
/// The letters typed are picked out in each label, and a key is a chip
/// at the row's end. What a narrow box cannot fit is left out whole - the
/// description first, then the chip - and never cut.
#[allow(clippy::too_many_arguments)]
pub fn inside(
    mode: &str,
    list: &[Row],
    total: usize,
    selected: usize,
    first: usize,
    query: &str,
    iw: usize,
    ih: usize,
) -> Vec<String> {
    let bar = picker::modal_ground();
    let band = theme::tint(Role::Slot);
    let line = |parts: &[(String, String)], ground: &str| -> String {
        let used: usize = parts.iter().map(|(_, t)| crate::display_width(t)).sum();
        let mut segs: Vec<(String, String)> = parts.to_vec();
        if used < iw {
            segs.push(("\x1b[22;23m".to_string(), " ".repeat(iw - used)));
        }
        let refs: Vec<(&str, String)> = segs.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
        let body = crate::seg(&refs, iw).replace("\x1b[0m", &format!("\x1b[0m{ground}"));
        format!("{ground}{body}\x1b[0m")
    };
    // `seg` does not reset between segments, so each says its own weight.
    let plain = |ink: String| format!("\x1b[22;23m{ink}");
    let bold = |ink: String| format!("\x1b[23;1m{ink}");
    // Back to a row's own ground after a chip: the default background
    // where the row has none, so a chip's tint never runs on.
    let back = |ground: &str| if ground.is_empty() { "\x1b[49m".to_string() } else { ground.to_string() };
    let mut rows = Vec::new();

    let shown = list.iter().filter(|r| r.runs != Runs::Header).count();
    let count = format!("{shown} of {total} ");
    let title = format!(" ◆ {mode}");
    let gap = iw.saturating_sub(crate::display_width(&title) + crate::display_width(&count)).max(1);
    rows.push(line(
        &[
            (bold(theme::ink_on(Role::Accent, true)), title),
            (plain(String::new()), " ".repeat(gap)),
            (plain(theme::ink_on(Role::Muted, true)), count),
        ],
        &band,
    ));

    let mut filter = vec![
        (bold(theme::ink(Role::Accent)), "  ❯ ".to_string()),
        (plain(theme::ink(Role::Fg)), query.to_string()),
        (plain(theme::ink(Role::Accent)), "▏".to_string()),
    ];
    if query.is_empty() {
        filter.push((format!("\x1b[22;3m{}", theme::ink(Role::Muted)), "type to filter".into()));
    }
    rows.push(line(&filter, &bar));
    rows.push(line(
        &[(plain(theme::ink(Role::Border)), format!(" {} ", "─".repeat(iw.saturating_sub(2))))],
        &bar,
    ));

    let slots = slots(ih);
    let first = first.min(list.len().saturating_sub(slots));
    let mut listed = Vec::new();
    if list.is_empty() {
        listed.push(line(
            &[(format!("\x1b[22;3m{}", theme::ink(Role::Muted)), "   nothing matches".into())],
            &bar,
        ));
    }
    for (i, r) in list.iter().enumerate().skip(first).take(slots) {
        if r.runs == Runs::Header {
            let head = format!("  {} ", r.label);
            let fill = iw.saturating_sub(crate::display_width(&head) + 1);
            listed.push(line(
                &[
                    (bold(theme::ink(Role::Muted)), head),
                    (plain(theme::ink(Role::Border)), "─".repeat(fill)),
                ],
                &bar,
            ));
            continue;
        }
        let here = i == selected;
        let ground = if here { theme::tint(Role::Slot) } else { bar.clone() };
        let chip_ground = if here { bar.clone() } else { theme::tint(Role::Slot) };
        let mark = if here {
            (plain(theme::ink_on(Role::Accent, true)), "▌ ".to_string())
        } else {
            (plain(String::new()), "  ".to_string())
        };
        let label_w = crate::display_width(&r.label);
        let mut chip = r.key.map(|k| format!(" {k} ")).unwrap_or_default();
        let mut detail = if r.detail.is_empty() { String::new() } else { format!("  {}", r.detail) };
        // The chip keeps a cell clear of the box's edge.
        let chip_room = |chip: &str| if chip.is_empty() { 0 } else { crate::display_width(chip) + 1 };
        if 2 + label_w + chip_room(&chip) > iw {
            chip.clear();
        }
        if 2 + label_w + crate::display_width(&detail) + chip_room(&chip) > iw {
            detail.clear();
        }
        let mut parts = vec![mark];
        // The label, with the letters typed picked out.
        let hits = matched(query, &r.label);
        let mut run = String::new();
        let mut run_hit = false;
        let label_ink = |hit: bool| -> String {
            if hit {
                bold(theme::ink_on(Role::Accent, here))
            } else if here {
                bold(theme::ink_on(Role::Fg, true))
            } else {
                plain(theme::ink(Role::Fg))
            }
        };
        for (n, c) in r.label.chars().enumerate() {
            let hit = hits.get(n).copied().unwrap_or(false);
            if !run.is_empty() && hit != run_hit {
                parts.push((label_ink(run_hit), std::mem::take(&mut run)));
            }
            run_hit = hit;
            run.push(c);
        }
        if !run.is_empty() {
            parts.push((label_ink(run_hit), run));
        }
        parts.push((plain(theme::ink_on(Role::Muted, here)), detail.clone()));
        if !chip.is_empty() {
            let used = 2 + label_w + crate::display_width(&detail);
            let gap = iw.saturating_sub(used + chip_room(&chip));
            parts.push((plain(String::new()), " ".repeat(gap)));
            let chip_ink = if here { theme::ink(Role::Muted) } else { theme::ink_on(Role::Muted, true) };
            parts.push((format!("\x1b[22;23m{chip_ink}{chip_ground}"), chip));
            parts.push((plain(back(&ground)), " ".to_string()));
        }
        listed.push(line(&parts, &ground));
    }
    while listed.len() < slots {
        listed.push(line(&[], &bar));
    }
    rows.extend(listed);

    let words = keys_with_words(iw);
    let mut keys = vec![(plain(String::new()), " ".to_string())];
    for (n, (key, word)) in KEYS.iter().enumerate() {
        keys.push((format!("\x1b[22;23m{}{}", theme::ink_on(Role::Muted, true), band), format!(" {key} ")));
        let after = if words { format!(" {word}") } else { String::new() };
        keys.push((plain(format!("{}{}", back(&bar), theme::ink(Role::Muted))), after));
        if n + 1 < KEYS.len() {
            keys.push((plain(String::new()), if words { "   " } else { " " }.to_string()));
        }
    }
    rows.push(line(&keys, &bar));
    rows.truncate(ih);
    rows
}

/// The pixel half of the box's inside, for Kitty: the rule under the
/// filter line as a hairline, and the chosen row's bar as a crisp stripe
/// down its left edge. In a text terminal each draws as the characters
/// [`inside`] already put there, so the two read the same.
pub fn marks(m: Modal, list: &[Row], selected: usize, first: usize) -> Vec<crate::Plot> {
    use crate::Plot;
    let iw = m.w.saturating_sub(2);
    let ih = m.h.saturating_sub(2);
    let ground = picker::modal_ground();
    let mut plots = Vec::new();
    if ground.is_empty() || iw < 3 {
        return plots;
    }
    let border = theme::ink(Role::Border);
    let t = 0.08;
    plots.push(Plot::filled(
        m.x + 1,
        m.y + 1 + 2,
        iw,
        1,
        vec![
            Plot::rect(0.0, 0.0, iw as f64, 1.0, &ground),
            Plot::rect(1.0, 0.5 - t / 2.0, iw as f64 - 2.0, t, &border),
        ],
        vec![vec![(format!("{border}{ground}"), format!(" {} ", "─".repeat(iw - 2)))]],
    ));
    let slots = slots(ih);
    let first = first.min(list.len().saturating_sub(slots));
    if selected >= first && selected < first + slots && list.get(selected).is_some_and(|r| r.runs != Runs::Header) {
        let slot = theme::tint(Role::Slot);
        let accent = theme::ink_on(Role::Accent, true);
        plots.push(Plot::filled(
            m.x + 1,
            m.y + 1 + 3 + (selected - first),
            1,
            1,
            vec![Plot::rect(0.0, 0.0, 1.0, 1.0, &slot), Plot::rect(0.0, 0.0, 0.28, 1.0, &accent)],
            vec![vec![(format!("{accent}{slot}"), "▌".to_string())]],
        ));
    }
    plots
}

/// What a click at `col`, `row` of a pane `w` by `h` means while the box
/// shows `list` scrolled to `first`: the key it stands for. Outside the
/// box it is esc - a click away from the palette is backing out of it,
/// the same reading as the picker. On a row it is `pick:<index>`, which
/// `run` turns into a move or, on the row already chosen, into ↵ - the
/// same as a click on a widget's row.
pub fn click_key(col: usize, row: usize, w: usize, h: usize, list: &[Row], first: usize) -> Option<String> {
    let m = Modal::place(w, h);
    if col < m.x || col >= m.x + m.w || row < m.y || row >= m.y + m.h {
        return Some("esc".into());
    }
    let ih = m.h.saturating_sub(2);
    let inner_row = row.checked_sub(m.y + 1)?;
    let inner_col = col.checked_sub(m.x + 1)?;
    if inner_row >= ih || inner_col >= m.w.saturating_sub(2) {
        return None;
    }
    if inner_row + 1 == ih {
        return key_spans(m.w.saturating_sub(2))
            .into_iter()
            .find(|(_, from, to)| (*from..*to).contains(&inner_col))
            .and_then(|(key, _, _)| match key {
                "↵" => Some("enter".to_string()),
                "esc" => Some("esc".to_string()),
                _ => None,
            });
    }
    let slot = inner_row.checked_sub(AROUND - 1)?;
    let slots = slots(ih);
    if slot >= slots {
        return None;
    }
    let first = first.min(list.len().saturating_sub(slots));
    let at = first + slot;
    // A heading is not a command: a click on one does nothing.
    (list.get(at).is_some_and(|r| r.runs != Runs::Header)).then(|| format!("pick:{at}"))
}

/// The first drawn row of a window `slots` tall that keeps `selected` in
/// view, moving from `first` no further than it must. The list is flat -
/// no section rules to step over - so the follow is the arithmetic.
pub fn follow_flat(selected: usize, slots: usize, first: usize) -> usize {
    let slots = slots.max(1);
    if selected < first {
        selected
    } else if selected >= first + slots {
        selected + 1 - slots
    } else {
        first
    }
}

/// The furthest the flat list can be scrolled: its last row on the
/// window's last.
pub fn last_flat(len: usize, slots: usize) -> usize {
    len.saturating_sub(slots.max(1))
}

/// Where the window starts once a key has moved the highlight, and on the
/// frame the box opens. [`follow_flat`] keeps the chosen command in view.
/// A section heading just above that command comes with it, but only when
/// the window has a row spare for it: a one-row list that steps back onto
/// the heading hides the command, and ↵ then runs what is not on screen.
pub fn follow_command(list: &[Row], selected: usize, slots: usize, first: usize) -> usize {
    let slots = slots.max(1);
    let mut first = follow_flat(selected, slots, first);
    if slots > 1
        && first == selected
        && first > 0
        && list.get(first - 1).is_some_and(|r| r.runs == Runs::Header)
    {
        first -= 1;
    }
    first.min(last_flat(list.len(), slots))
}

/// Key names already read, handed to the widget that replaces this one.
/// Newline-separated. The new process reads them once, at start, and
/// removes the variable, so a later restart does not play them again and
/// a failed switch - which stays in this process - never sees them.
const HANDOFF_KEYS: &str = "OPSCOPE_HANDOFF_KEYS";

/// `keys` joined for [`HANDOFF_KEYS`]. `None` when there is nothing to
/// hand over, so the variable is not set at all. A name containing a
/// newline cannot travel this way and is dropped rather than split.
pub(crate) fn encode_handoff(keys: &[String]) -> Option<String> {
    let joined: Vec<&str> = keys
        .iter()
        .map(String::as_str)
        .filter(|k| !k.is_empty() && !k.contains('\n'))
        .collect();
    (!joined.is_empty()).then(|| joined.join("\n"))
}

/// The keys [`encode_handoff`] wrote, in the order they were read.
pub(crate) fn decode_handoff(raw: &str) -> Vec<String> {
    raw.split('\n').filter(|k| !k.is_empty()).map(str::to_string).collect()
}

/// Take [`HANDOFF_KEYS`] out of the environment. Called once, when the
/// keyboard is built, so the keys are this process's and nobody else's.
pub(crate) fn take_handoff_keys() -> Vec<String> {
    let raw = std::env::var(HANDOFF_KEYS).unwrap_or_default();
    std::env::remove_var(HANDOFF_KEYS);
    decode_handoff(&raw)
}

/// One mode's list state: what was typed, where the highlight is, and
/// where its window sits. Kept per mode, so esc back to the commands
/// finds them where they were left rather than reset.
#[derive(Default)]
struct List {
    query: String,
    selected: usize,
    first: usize,
}

/// Switch this pane to the widget `stem` beside this one: this widget ends
/// and that one takes its place, by `exec`, the way a restart does.
///
/// Not a child it waits for. Running the other widget as a child put this
/// one back on screen when it quit - a stack of widgets, one more each
/// switch, that quitting peeled one at a time. Replaced, quitting the new
/// widget leaves the pane, or goes back to the `opscope` menu when that
/// is what started this one, the same as quitting this widget would.
///
/// `exec` returns only when it failed - the binary is not beside this one,
/// or cannot run. Then the reason is shown, the terminal is taken back,
/// and the palette is still open over this widget. `keys` were already
/// read from the terminal, so they go with the new process; a failed
/// switch drops them rather than typing them into this filter afterwards.
fn switch_to(keyboard: &mut crate::Keyboard, stem: &str, keys: &[String]) {
    use std::os::unix::process::CommandExt;

    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(stem)));
    keyboard.restore();
    crate::restore_screen();
    let why = match beside {
        Some(path) => {
            let mut command = std::process::Command::new(&path);
            if let Some(held) = encode_handoff(keys) {
                command.env(HANDOFF_KEYS, held);
            }
            let err = command.exec();
            format!("{}: {}", path.display(), err)
        }
        None => "cannot find where this binary lives".to_string(),
    };
    crate::out(&format!("{why}\r\n"));
    crate::flush();
    std::thread::sleep(Duration::from_secs(2));
    // Take the terminal back rather than assuming: cbreak again, cursor
    // away again, screen clear, mouse reporting on again, without which
    // the wheel does nothing on the way back in.
    keyboard.reclaim();
    crate::claim_screen();
}

/// Run the palette over the widget's last frame until it is escaped out
/// of. Every mode redraws over that same frame, so the theme picker, the
/// widget list and the help swap into this box in place - one surface, the
/// way a palette mode changes - never a stack of boxes.
///
/// Returns a key when the command run was one of the widget's own footer
/// actions: `poll` hands it to the widget as if it were typed.
pub(crate) fn run(keyboard: &mut crate::Keyboard) -> Option<String> {
    if OPEN.swap(true, Ordering::AcqRel) {
        return None;
    }
    // The widget's own actions, read before its footer is forgotten.
    let actions = keyboard.footer_actions();
    // The widget's footer is under the box now. Left bound, a click on
    // where its `↵ open` was would come back as that key, where it is
    // outside the box and means esc.
    keyboard.forget_footer();
    let (base_rows, base_plots) = crate::LAST_FRAME
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_default();
    let charts = Charts::now();
    let recent_ids = recent();
    let commands = by_recent(commands(&actions, charts), &recent_ids);
    let switch = switch_rows();
    let help_text = crate::HELP_TEXT.get().map(String::as_str).unwrap_or("No help text for this binary.");
    let mut help = help_rows(help_text, 40);
    let mut help_width = 0usize;
    let mut commands_list = List::default();
    let mut switch_list = List::default();
    let mut help_list = List::default();
    // Which mode the box is in. The theme picker is a mode too, but it
    // runs as itself, over the same base, and returns here when it is
    // escaped out of.
    let mut mode = Mode::Commands;
    // What the last command did, when it has nothing else to show: the
    // copies say so here, in the title.
    let mut note: Option<String> = None;
    // The first frame puts the highlight in the window. Later frames
    // follow only when a key moved it, so the wheel can leave it off screen.
    let mut placed = false;
    loop {
        let (w, h) = crate::size();
        let m = Modal::place(w, h);
        // The help is wrapped to the box, and the box follows the pane.
        let inner = m.w.saturating_sub(2);
        if inner != help_width {
            help_width = inner;
            help = help_rows(help_text, inner.saturating_sub(4));
        }
        let query = match mode {
            Mode::Commands => commands_list.query.clone(),
            Mode::Widgets => switch_list.query.clone(),
            Mode::Help => help_list.query.clone(),
        };
        let mut list = refilter(&commands, &switch, &help, mode, &query, &recent_ids);
        let total = match mode {
            Mode::Commands => commands.len(),
            Mode::Widgets => switch.len(),
            Mode::Help => help.len(),
        };
        let room = m.h.saturating_sub(2);
        let window = slots(room);
        let current = match mode {
            Mode::Commands => &mut commands_list,
            Mode::Widgets => &mut switch_list,
            Mode::Help => &mut help_list,
        };
        if current.selected >= list.len() {
            current.selected = list.len().saturating_sub(1);
        }
        current.selected = settle(&list, current.selected, true);
        current.first = current.first.min(last_flat(list.len(), window));
        if !placed {
            current.first = follow_command(&list, current.selected, window, current.first);
            placed = true;
        }
        let (selected_now, first_now) = (current.selected, current.first);
        let title = match (mode, &note) {
            (Mode::Commands, Some(n)) => format!("Commands · {n}"),
            (Mode::Commands, None) => "Commands".to_string(),
            (Mode::Widgets, _) => "Widgets".to_string(),
            (Mode::Help, _) => "Keys & help".to_string(),
        };
        let boxed = picker::framed(
            &inside(
                &title,
                &list,
                total,
                current.selected,
                current.first,
                &current.query,
                m.w.saturating_sub(2),
                room,
            ),
            m.w.saturating_sub(2),
        );
        // The widget's frame was built in the theme it started in; the
        // box is built in the one in force.
        let mut rows: Vec<String> = base_rows.iter().take(h).map(|r| theme::recolour(r)).collect();
        rows.resize(h, String::new());
        for (i, row) in boxed.iter().enumerate() {
            if let Some(base) = rows.get_mut(m.y + i) {
                *base = picker::splice(base, m.x, row, m.w, w);
            }
        }
        // The widget's own charts stay where the box and its shadow are
        // not; one under the box would be drawn over it.
        let (bx, by, bw, bh) = (m.x, m.y, m.w + 1, m.h + 1);
        let mut plots: Vec<crate::Plot> = base_plots
            .iter()
            .filter(|p| p.x >= bx + bw || p.x + p.width <= bx || p.y >= by + bh || p.y + p.height <= by)
            .cloned()
            .map(|p| p.recoloured(&theme::recolour))
            .collect();
        plots.extend(picker::chrome(m, w, h));
        plots.extend(marks(m, &list, selected_now, first_now));
        crate::draw_current(&rows, w, h, &plots);
        keyboard.typing();
        let keys = keyboard.poll();
        // Whether a key moved the highlight on this pass. Only that
        // brings the window along - the wheel writes `first` and
        // nothing else, or scrolling to look at a command would drag
        // the view straight back to the highlight.
        let mut moved = false;
        // The batch walked by index, so a command that opens a screen can
        // hand that screen the rest of the batch first. Those keys were
        // typed for the screen - `\rdrac` means "theme picker, filtered to
        // drac" - and would otherwise sit here and land on the commands
        // filter after the screen closed.
        let mut pending = keys;
        let mut at = 0;
        while at < pending.len() {
            let key = pending[at].clone();
            at += 1;
            // The mode can change inside the batch (Switch widget…), so
            // the state and the rows are taken again for every key
            // rather than once per poll.
            let state = match mode {
                Mode::Commands => &mut commands_list,
                Mode::Widgets => &mut switch_list,
                Mode::Help => &mut help_list,
            };
            let selected = state.selected;
            let first = state.first;
            // A click is the key it stands for: outside the box it is
            // esc, on the keys row the key it names, and on a row a
            // move there - or ↵, when that row is already the one
            // chosen. Rewritten rather than acted on, so the arm below
            // does the running and the mouse cannot drift from the
            // keyboard.
            let key = match crate::click_at(&key) {
                Some((col, row)) => match click_key(col, row, w, h, &list, first) {
                    Some(k) => match k.strip_prefix("pick:").and_then(|i| i.parse::<usize>().ok()) {
                        Some(i) if i == selected => "enter".to_string(),
                        Some(i) => {
                            state.selected = settle(&list, i, true);
                            moved = true;
                            continue;
                        }
                        None => k,
                    },
                    None => continue,
                },
                None => key,
            };
            // Whether this key changed what the list holds - the query
            // or the mode. Either refilters before the next key of the
            // same batch, so a paste of `not` then ↵ runs Notices, not
            // whatever `not` had not yet replaced.
            let mut changed = false;
            match key.as_str() {
                // The wheel and ctrl-y/ctrl-e move the view, never the
                // highlight, so scrolling never changes what ↵ runs.
                "wheel-up" => state.first = state.first.saturating_sub(3),
                "wheel-down" => state.first = (state.first + 3).min(last_flat(list.len(), window)),
                "ctrl-y" => state.first = state.first.saturating_sub(1),
                "ctrl-e" => state.first = (state.first + 1).min(last_flat(list.len(), window)),
                // esc pops one level: the widgets or the help back to the
                // commands, the commands back to the widget. ctrl-k is esc
                // here - the key that opened the palette closing it.
                "esc" | "ctrl-k" => {
                    if mode != Mode::Commands {
                        mode = Mode::Commands;
                        changed = true;
                    } else {
                        close();
                        return None;
                    }
                }
                "enter" => {
                    let row = list.get(selected).cloned();
                    if mode == Mode::Commands {
                        if let Some(r) = &row {
                            remember(&r.id());
                        }
                        note = None;
                    }
                    let mut opened = false;
                    match row.as_ref().map(|r| r.runs) {
                        Some(Runs::Theme) => {
                            keyboard.hold(pending[at..].to_vec());
                            picker::run_over(keyboard, &base_rows, &base_plots);
                            opened = true;
                        }
                        Some(Runs::Settings) => {
                            if let Some(spec) = settings::registered() {
                                keyboard.hold(pending[at..].to_vec());
                                // Relaunches itself by exec on a save;
                                // what returns here is a save not made.
                                settings::run_settings(keyboard, spec);
                                opened = true;
                            }
                        }
                        Some(Runs::Notices) => {
                            keyboard.hold(pending[at..].to_vec());
                            crate::notices::run(keyboard);
                            opened = true;
                        }
                        Some(Runs::Widgets) => {
                            mode = Mode::Widgets;
                            changed = true;
                        }
                        Some(Runs::Help) => {
                            mode = Mode::Help;
                            changed = true;
                        }
                        Some(Runs::Widget) => {
                            if let Some(r) = &row {
                                // Already read, so the new process cannot
                                // see them on the terminal. Handed across,
                                // and cleared here so a failed switch does
                                // not type them into this filter.
                                let rest = pending[at..].to_vec();
                                pending.clear();
                                switch_to(keyboard, &r.label, &rest);
                            }
                        }
                        Some(Runs::Send) => {
                            // The rest of the batch was typed after the
                            // action, so it is the widget's too.
                            keyboard.hold(pending[at..].to_vec());
                            close();
                            return row.and_then(|r| r.sends);
                        }
                        Some(Runs::Graphics) => {
                            let to = if charts == Charts::ToPixels { "kitty" } else { "text" };
                            close();
                            // Returns only when the restart failed: the
                            // widget goes on as it was.
                            settings::relaunch_with(keyboard, &[("OPSCOPE_GRAPHICS", to)]);
                            return None;
                        }
                        Some(Runs::Restart) => {
                            close();
                            settings::relaunch(keyboard);
                            return None;
                        }
                        Some(Runs::CopyDiagnostics) => {
                            let text = diagnostics(|k| std::env::var(k).ok(), w, h);
                            note = Some(copied(crate::clipboard(&text), "diagnostics"));
                        }
                        Some(Runs::CopyDocs) => {
                            note = Some(copied(crate::clipboard(&docs_link(&this_stem())), "docs link"));
                        }
                        Some(Runs::Line) | Some(Runs::Header) | None => {}
                    }
                    if opened {
                        // The screen took what it wanted from the batch;
                        // the rest was typed for it and is spent.
                        pending.clear();
                    }
                }
                // The highlight steps over the section headings.
                "up" => {
                    state.selected = settle(&list, state.selected.saturating_sub(1), false);
                    moved = true;
                }
                "down" => {
                    state.selected = settle(&list, (state.selected + 1).min(list.len().saturating_sub(1)), true);
                    moved = true;
                }
                "pgup" => {
                    state.selected = settle(&list, state.selected.saturating_sub(window.max(1)), false);
                    moved = true;
                }
                "pgdn" => {
                    state.selected =
                        settle(&list, (state.selected + window.max(1)).min(list.len().saturating_sub(1)), true);
                    moved = true;
                }
                "home" => {
                    state.selected = settle(&list, 0, true);
                    moved = true;
                }
                "end" => {
                    state.selected = settle(&list, list.len().saturating_sub(1), false);
                    moved = true;
                }
                "backspace" => {
                    state.query.pop();
                    state.selected = 0;
                    moved = true;
                    changed = true;
                }
                other if other.chars().count() == 1 && !other.chars().all(char::is_control) => {
                    state.query.push_str(other);
                    state.selected = 0;
                    moved = true;
                    changed = true;
                }
                _ => {}
            }
            if changed {
                let state = match mode {
                    Mode::Commands => &mut commands_list,
                    Mode::Widgets => &mut switch_list,
                    Mode::Help => &mut help_list,
                };
                list = refilter(&commands, &switch, &help, mode, &state.query, &recent_ids);
                state.selected = settle(&list, state.selected.min(list.len().saturating_sub(1)), true);
            }
        }
        // On the pass a key moved the highlight, the window follows it.
        let current = match mode {
            Mode::Commands => &mut commands_list,
            Mode::Widgets => &mut switch_list,
            Mode::Help => &mut help_list,
        };
        if moved {
            current.first = follow_command(&list, current.selected, window, current.first);
        }
        current.first = current.first.min(last_flat(list.len(), window));
        keyboard.wait(Duration::from_millis(60));
    }
}

/// Which list the box is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Commands,
    Widgets,
    Help,
}

/// What the title says after a copy: that it was sent, and to where. OSC 52
/// is the terminal's to honour, so "sent" is as far as this can know.
fn copied(sent: bool, what: &str) -> String {
    if sent {
        format!("{what} sent to the clipboard")
    } else {
        format!("{what} not copied: not a terminal")
    }
}

fn close() {
    OPEN.store(false, Ordering::Release);
    crate::forget_frame();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_commands_are_the_widget_wide_actions() {
        let rows = commands(&[], Charts::ToText);
        assert_eq!(rows[0].label, "Theme…");
        assert_eq!(rows[0].runs, Runs::Theme);
        assert!(rows.iter().any(|r| r.label == "Switch widget…" && r.runs == Runs::Widgets));
        assert!(rows.iter().any(|r| r.label == "Notices" && r.key == Some("!")));
        // Settings only when this binary registered one, and the test
        // binary registers none - as the launcher and matrix never do.
        assert!(settings::registered().is_none(), "the test binary registers nothing");
        assert!(!rows.iter().any(|r| r.runs == Runs::Settings));
    }

    #[test]
    fn filtering_matches_label_key_and_detail() {
        // A fixture shaped like the real list but with the Settings row
        // in it, which the test binary's own commands() cannot have:
        // nothing registers a spec under test.
        let row = |label: &str, detail: &str, key: Option<&'static str>, runs: Runs| Row {
            label: label.into(),
            detail: detail.into(),
            key,
            runs,
            sends: None,
        };
        let rows = vec![
            row("Theme…", "pick a theme for this pane", None, Runs::Theme),
            row("Settings…", "this widget's settings", Some(","), Runs::Settings),
            row("Switch widget…", "replace this widget with another", None, Runs::Widgets),
            row("Notices", "warnings this widget raised", Some("!"), Runs::Notices),
        ];
        let set = |q: &str| matching(&rows, q).iter().map(|r| r.label.clone()).collect::<Vec<_>>();
        assert!(set("set").contains(&"Settings…".to_string()));
        assert!(set("the").contains(&"Theme…".to_string()));
        assert!(set("swi").contains(&"Switch widget…".to_string()));
        assert!(set("not").contains(&"Notices".to_string()));
        assert!(matching(&rows, "zzzz").is_empty());
        assert_eq!(matching(&rows, "").len(), rows.len());
    }

    #[test]
    fn a_widgets_footer_actions_are_commands_that_send_their_key() {
        let actions = vec![
            ("r".to_string(), "[r]estart".to_string()),
            ("b".to_string(), "[b]reak start".to_string()),
            // Core's own keys have rows already; a hint for one is not
            // listed again, and a key twice in a footer is listed once.
            (",".to_string(), "[,] settings".to_string()),
            ("ctrl-k".to_string(), "[ctrl-k]cmds".to_string()),
            ("ctrl-t".to_string(), "[ctrl-t]heme".to_string()),
            ("r".to_string(), "[r]estart".to_string()),
        ];
        let rows = commands(&actions, Charts::ToText);
        let sent: Vec<(&str, &str)> = rows
            .iter()
            .filter(|r| r.runs == Runs::Send)
            .map(|r| (r.label.as_str(), r.sends.as_deref().unwrap()))
            .collect();
        assert_eq!(sent, vec![("[r]estart", "r"), ("[b]reak start", "b")]);
        // Typing a word of the hint finds it.
        assert_eq!(matching(&rows, "break")[0].label, "[b]reak start");
    }

    #[test]
    fn the_chart_switch_names_where_it_goes_and_only_where_it_can() {
        let has = |rows: &[Row], l: &str| rows.iter().any(|r| r.label == l);
        let pixels = commands(&[], Charts::ToText);
        assert!(has(&pixels, "Charts as text") && !has(&pixels, "Charts as pixels"));
        let asked_text = commands(&[], Charts::ToPixels);
        assert!(has(&asked_text, "Charts as pixels") && !has(&asked_text, "Charts as text"));
        // Text only because the terminal draws no images, or NO_COLOR:
        // nothing to switch to.
        let cannot = commands(&[], Charts::Neither);
        assert!(!has(&cannot, "Charts as pixels") && !has(&cannot, "Charts as text"));
        for r in ["Keys & help…", "Restart widget", "Copy diagnostics", "Copy docs link"] {
            assert!(has(&pixels, r), "{r}");
        }
    }

    #[test]
    fn recent_commands_come_first_and_the_rest_keep_their_order() {
        let rows = commands(&[], Charts::ToText);
        let recent = remembered(remembered(Vec::new(), "Notices"), "Restart widget");
        // A widget action is remembered by its key, so a toggle whose text
        // changed is still found.
        let p_on = Row { label: "[p]omodoro".into(), detail: String::new(), key: None, runs: Runs::Send, sends: Some("p".into()) };
        let p_off = Row { label: "[p]omodoro off".into(), ..p_on.clone() };
        assert_eq!(by_recent(vec![p_off.clone()], &[p_on.id()])[0].label, "[p]omodoro off");
        assert_eq!(recent, vec!["Restart widget".to_string(), "Notices".to_string()]);
        let ordered = by_recent(rows.clone(), &recent);
        assert_eq!(ordered[0].label, "Restart widget");
        assert_eq!(ordered[1].label, "Notices");
        let rest: Vec<&str> = ordered[2..].iter().map(|r| r.label.as_str()).collect();
        let expected: Vec<&str> = rows
            .iter()
            .map(|r| r.label.as_str())
            .filter(|l| *l != "Restart widget" && *l != "Notices")
            .collect();
        assert_eq!(rest, expected);
        // A label no longer offered is skipped, and the list is kept short.
        assert_eq!(by_recent(rows.clone(), &["Gone".to_string()]).len(), rows.len());
        let mut many = Vec::new();
        for i in 0..9 {
            many = remembered(many, &format!("c{i}"));
        }
        assert_eq!(many.len(), RECENT);
        assert_eq!(many[0], "c8");
    }

    #[test]
    fn help_is_its_lines_and_diagnostics_hold_no_path() {
        let rows = help_rows("clocks - a clock\n\n  q  quit\n  r  restart\n", 60);
        assert_eq!(rows.len(), 3);
        // Wrapped, never cut, with the indent kept on the lines it wraps onto.
        let long = "  the pomodoro persists across restarts so relaunching the panel does not cost you a session";
        let wrapped = help_rows(long, 30);
        assert!(wrapped.len() > 1);
        for r in &wrapped {
            assert!(crate::display_width(&r.label) <= 30, "{:?}", r.label);
            assert!(r.label.starts_with("  "), "{:?}", r.label);
        }
        let words: Vec<&str> = wrapped.iter().flat_map(|r| r.label.split_whitespace()).collect();
        assert_eq!(words, long.split_whitespace().collect::<Vec<_>>());
        // Prose wrapped for 80 columns is rejoined before it is wrapped
        // again, so no word is left alone on a row; a key table is not.
        let text = "A big clock and countdown bars for the next\nhour, and the end of day.\n\n  q  quit\n  r  restart\n";
        let rows = help_rows(text, 70);
        assert_eq!(rows[0].label, "A big clock and countdown bars for the next hour, and the end of day.");
        assert_eq!(rows[1].label, "  q  quit");
        assert_eq!(rows[2].label, "  r  restart");
        assert!(rows.iter().all(|r| r.runs == Runs::Line));
        assert_eq!(matching(&rows, "restart").len(), 1);
        let env = |k: &str| match k {
            "TERM" => Some("xterm-256color".to_string()),
            "REX_SESSION" => Some("s".to_string()),
            "HOME" => Some("/home/someone".to_string()),
            _ => None,
        };
        let d = diagnostics(env, 90, 34);
        assert!(d.contains("theme: ") && d.contains("multiplexer: Rex") && d.contains("pane: 90x34"), "{d}");
        assert!(d.contains("TERM=xterm-256color"), "{d}");
        assert!(!d.contains("/home"), "a path in the diagnostics: {d}");
        assert_eq!(docs_link("clocks"), "https://github.com/stealth-factory/opscope/tree/main/widgets/src/widgets/clocks");
        assert_eq!(docs_link("opscope"), "https://github.com/stealth-factory/opscope#readme");
    }

    #[test]
    fn the_manifest_names_every_binary_and_the_launcher_is_not_a_widget() {
        let names = built();
        // Nineteen widgets and the launcher.
        assert_eq!(names.len(), 20, "{names:?}");
        assert!(names.contains(&"clocks".to_string()));
        assert!(names.contains(&"vercel-deployments".to_string()));
        assert!(names.contains(&"google-trends".to_string()));
        // Alphabetical, as the launcher lists them.
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
    }

    #[test]
    fn switch_offers_every_widget_but_the_launcher_and_this_one() {
        let stems = others();
        assert!(!stems.contains(&"opscope".to_string()), "the launcher is not a widget to start");
        // The test binary is not one of the manifest's names, so only
        // the launcher drops out here; a running widget drops two.
        assert_eq!(stems.len(), built().len() - 1, "{stems:?}");
        let rows = switch_rows();
        assert_eq!(rows.len(), stems.len());
        assert!(rows.iter().all(|r| r.runs == Runs::Widget));
    }

    #[test]
    fn a_query_and_enter_in_one_batch_run_the_filtered_row() {
        // `not` and ↵ can arrive in one poll. The list ↵ acts on is the
        // one refiltered after `not`, not the list the batch opened on.
        let commands = commands(&[], Charts::ToText);
        let switch = switch_rows();
        let help = help_rows("q quit", 40);
        let before = refilter(&commands, &switch, &help, Mode::Commands, "", &[]);
        assert_eq!(before[0].runs, Runs::Header, "nothing typed: in sections");
        assert_eq!(before[settle(&before, 0, true)].label, "Theme…");
        let after = refilter(&commands, &switch, &help, Mode::Commands, "not", &[]);
        assert_eq!(after.first().map(|r| r.label.clone()), Some("Notices".to_string()));
        // A mode change refilters too, against the other list.
        let widgets = refilter(&commands, &switch, &help, Mode::Widgets, "", &[]);
        assert!(widgets.iter().all(|r| r.runs == Runs::Widget));
        assert_eq!(widgets.len(), switch.len());
        let lines = refilter(&commands, &switch, &help, Mode::Help, "quit", &[]);
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn the_box_fits_every_pane_and_answers_clicks() {
        let rows = commands(&[], Charts::ToText);
        for (w, h) in [(30, 10), (40, 12), (80, 24), (140, 50), (24, 8)] {
            let m = Modal::place(w, h);
            assert!(m.x + m.w < w && m.y >= 1 && m.y + m.h <= h, "{w}x{h}: {m:?}");
            let inner = inside("Commands", &rows, rows.len(), 0, 0, "", m.w - 2, m.h - 2);
            assert_eq!(inner.len(), m.h - 2, "{w}x{h}");
            for r in &inner {
                assert_eq!(crate::display_width(r), m.w - 2, "{w}x{h}: {r:?}");
            }
            for r in picker::framed(&inner, m.w - 2) {
                assert_eq!(crate::display_width(&r), m.w, "{w}x{h}");
            }
            // A click on the first option picks it and - where the box
            // has room for two - the next option is on the row under it.
            let col = m.x + 3;
            assert_eq!(click_key(col, m.y + 4, w, h, &rows, 0).as_deref(), Some("pick:0"));
            if slots(m.h - 2) >= 2 {
                assert_eq!(click_key(col, m.y + 5, w, h, &rows, 0).as_deref(), Some("pick:1"));
            }
            // The title, the filter line and the rule are not options.
            for r in 1..=3 {
                assert_eq!(click_key(col, m.y + r, w, h, &rows, 0), None, "{w}x{h} row {r}");
            }
            assert_eq!(click_key(0, 0, w, h, &rows, 0).as_deref(), Some("esc"));
            // The keys row answers the keys it names, on its chips and
            // words alike, at whatever width it is drawn.
            let keys_row = m.y + m.h - 2;
            let spans = key_spans(m.w - 2);
            let at = |k: &str| spans.iter().find(|(key, _, _)| *key == k).map(|(_, from, _)| m.x + 1 + from + 1).unwrap();
            assert_eq!(click_key(at("↵"), keys_row, w, h, &rows, 0).as_deref(), Some("enter"));
            assert_eq!(click_key(at("esc"), keys_row, w, h, &rows, 0).as_deref(), Some("esc"));
            assert_eq!(click_key(at("↑↓"), keys_row, w, h, &rows, 0), None);
            // Every key named is on the row, never cut.
            let shown = crate::strip_escapes(&inner[inner.len() - 1]);
            for (key, _) in KEYS {
                assert!(shown.contains(key), "{w}x{h}: {shown:?} lost {key}");
            }
        }
    }

    #[test]
    fn the_options_sit_one_a_row_as_the_theme_list_does() {
        let rows = commands(&[], Charts::ToText);
        let inner = inside("Commands", &rows, rows.len(), 0, 0, "", 60, 20);
        // A row as it reaches the terminal, without the colour escapes.
        fn plain(row: &str) -> String {
            let mut out = String::new();
            let mut chars = row.chars();
            while let Some(c) = chars.next() {
                if c == '\u{1b}' {
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
        // Title band, filter, rule, then one option a row, with the same
        // line height as the theme list.
        assert!(plain(&inner[0]).contains("◆ Commands"), "{:?}", inner[0]);
        assert!(plain(&inner[1]).contains("❯") && plain(&inner[1]).contains("type to filter"), "{:?}", inner[1]);
        assert!(plain(&inner[2]).trim().chars().all(|c| c == '─'), "{:?}", inner[2]);
        assert!(plain(&inner[3]).contains("Theme…"), "{:?}", inner[3]);
        assert!(plain(&inner[4]).contains("Settings…") || plain(&inner[4]).contains("Switch widget…"), "{:?}", inner[4]);
        assert_eq!(slots(20), 16);
        // The chosen row carries the bar; its key is a chip at the end.
        assert!(plain(&inner[3]).starts_with("▌ Theme…"), "{:?}", plain(&inner[3]));
        assert!(plain(&inner[3]).trim_end().ends_with("ctrl-t"), "{:?}", plain(&inner[3]));
        assert!(!plain(&inner[4]).contains("▌"));
    }

    #[test]
    fn the_window_follows_the_highlight_only_as_far_as_it_must() {
        assert_eq!(follow_flat(5, 10, 2), 2);
        assert_eq!(follow_flat(20, 10, 0), 11);
        assert_eq!(follow_flat(0, 10, 8), 0);
        assert_eq!(last_flat(53, 10), 43);
    }

    #[test]
    fn a_one_row_window_keeps_the_command_and_not_the_heading_above_it() {
        let rows = commands(&[], Charts::ToText);
        let list = sectioned(&rows, &[]);
        assert_eq!(list[0].runs, Runs::Header);
        let selected = settle(&list, 0, true);
        assert_ne!(list[selected].runs, Runs::Header);
        // Opens, and Home from further down, on the command itself.
        assert_eq!(follow_command(&list, selected, 1, 0), selected);
        assert_eq!(follow_command(&list, selected, 1, 8), selected);
        // Two rows: the heading comes along, and the command stays in view.
        let first = follow_command(&list, selected, 2, selected);
        assert_eq!(first, selected - 1);
        assert_eq!(list[first].runs, Runs::Header);
        assert!(selected >= first && selected < first + 2);
        // A command that is not the first of its section does not step back.
        let later = settle(&list, selected + 1, true);
        assert_ne!(list[later - 1].runs, Runs::Header);
        assert_eq!(follow_command(&list, later, 2, later), later);
    }

    #[test]
    fn keys_read_before_a_switch_travel_with_the_new_widget() {
        assert_eq!(encode_handoff(&[]), None);
        assert_eq!(encode_handoff(&[String::new()]), None);
        let held = encode_handoff(&["q".into(), "enter".into()]).unwrap();
        assert_eq!(decode_handoff(&held), ["q", "enter"]);
        // A newline cannot be a separator and part of a name.
        assert_eq!(encode_handoff(&["has\nnewline".into(), "q".into()]).as_deref(), Some("q"));
    }

    #[test]
    fn a_description_that_does_not_fit_is_dropped_not_cut() {
        let rows = commands(&[], Charts::ToText);
        for iw in [30, 44, 60, 74] {
            let inner = inside("Commands", &rows, rows.len(), 0, 0, "", iw, 20);
            for (r, row) in rows.iter().zip(inner.iter().skip(3)) {
                let shown = crate::strip_escapes(row);
                // Either the whole description is there or none of it.
                let whole = shown.contains(&r.detail);
                let opening: String = r.detail.split_whitespace().take(2).collect::<Vec<_>>().join(" ");
                let none = !shown.contains(&opening);
                assert!(whole || none, "iw={iw}: {shown:?} cut {:?}", r.detail);
            }
        }
    }

    #[test]
    fn the_commands_open_in_sections_and_the_highlight_steps_over_headings() {
        let actions = vec![("b".to_string(), "[b]reak start".to_string())];
        let rows = commands(&actions, Charts::ToText);
        let recent = vec!["Notices".to_string()];
        let list = sectioned(&rows, &recent);
        let labels: Vec<&str> = list.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels[0], "RECENT");
        assert_eq!(labels[1], "Notices");
        assert_eq!(labels[2], "OPSCOPE");
        let own = labels.iter().position(|l| *l == "THIS WIDGET").unwrap();
        assert_eq!(labels[own + 1], "[b]reak start");
        // A command sits in one section only.
        assert_eq!(labels.iter().filter(|l| **l == "Notices").count(), 1);
        // Down from the last of one section lands on the first of the next;
        // up from there comes back. Headings are never chosen.
        assert_eq!(settle(&list, 2, true), 3);
        assert_eq!(settle(&list, 2, false), 1);
        assert_eq!(settle(&list, 0, true), 1);
        // A click on a heading is nothing.
        let (w, h) = (100, 36);
        let m = Modal::place(w, h);
        assert_eq!(click_key(m.x + 3, m.y + 4, w, h, &list, 0), None);
        assert_eq!(click_key(m.x + 3, m.y + 5, w, h, &list, 0).as_deref(), Some("pick:1"));
        // The count is of commands, not of headings.
        let inner = inside("Commands", &list, rows.len(), 1, 0, "", 70, 20);
        assert!(crate::strip_escapes(&inner[0]).contains(&format!("{} of {}", rows.len(), rows.len())));
    }

    #[test]
    fn the_letters_typed_are_picked_out() {
        assert_eq!(matched("swi", "Switch widget…")[..4], [true, true, true, false]);
        assert_eq!(matched("sw", "Switch widget…").iter().filter(|b| **b).count(), 2);
        // Not every letter in the label: nothing is picked out.
        assert!(matched("zz", "Theme…").iter().all(|b| !b));
        // Drawn bold in the accent; the plain row stays the same width.
        let rows = commands(&[], Charts::ToText);
        let inner = inside("Commands", &rows, rows.len(), 0, 0, "the", 70, 20);
        assert!(inner[3].contains("\x1b[23;1m"), "{:?}", inner[3]);
        assert_eq!(crate::display_width(&inner[3]), 70);
    }

    #[test]
    fn the_pixel_marks_sit_on_the_rule_and_the_chosen_row() {
        let rows = commands(&[], Charts::ToText);
        let (w, h) = (100, 36);
        let m = Modal::place(w, h);
        let marks = marks(m, &rows, 2, 0);
        // Tests draw in Classic, whose modal ground is a tint: both marks.
        assert_eq!(marks.len(), 2);
        assert_eq!((marks[0].y, marks[0].width), (m.y + 3, m.w - 2));
        assert_eq!((marks[1].x, marks[1].y), (m.x + 1, m.y + 4 + 2));
    }

    #[test]
    fn an_empty_list_says_so() {
        let rows: Vec<Row> = Vec::new();
        let inner = inside("Commands", &rows, 0, 0, 0, "zz", 60, 12);
        assert!(inner.iter().any(|r| r.contains("nothing matches")));
        assert!(inner.iter().any(|r| r.contains("Commands")));
    }
}