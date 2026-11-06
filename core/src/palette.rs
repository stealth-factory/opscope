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
//! with [`settings::register_settings`]), notices, and starting another
//! widget in this pane. A command that already has a key shows it - `,`
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

/// The box's last row. A click on `↵ run` or `esc back` is that key.
const KEYS_ROW: &str = " ↑↓ choose · ↵ run · esc back";

/// How many options the box's list shows, with the title, the filter line
/// and the keys row around it: one option a row, as the theme list has
/// them, so every mode of the box reads with the same line height.
pub fn slots(ih: usize) -> usize {
    ih.saturating_sub(3).max(1)
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
    /// Start the widget named by the row, in this pane.
    Widget,
}

/// The commands on offer. Settings only when this binary registered a
/// settings spec - the launcher and `matrix` do not, and a Settings
/// command that answered with nothing would be a hint bound to nothing.
pub fn commands() -> Vec<Row> {
    let row = |label: &str, detail: &str, key: Option<&'static str>, runs: Runs| Row {
        label: label.into(),
        detail: detail.into(),
        key,
        runs,
    };
    let mut rows = vec![row("Theme…", "pick a theme for this pane", Some("ctrl-t"), Runs::Theme)];
    if settings::registered().is_some() {
        rows.push(row("Settings…", "this widget's settings", Some(","), Runs::Settings));
    }
    rows.push(row("Switch widget…", "start another widget in this pane", None, Runs::Widgets));
    rows.push(row("Notices", "warnings this widget raised", Some("!"), Runs::Notices));
    rows
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
    let here = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.file_stem().map(|stem| stem.to_string_lossy().into_owned()))
        .unwrap_or_default();
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

/// The rows a batch should be working with after a key changes the query
/// or the mode: what matches the query now, in the mode now.
///
/// `run` calls this after every key that edits the filter or switches
/// mode, rather than trusting the list the batch opened with - a paste
/// of `not` and ↵ arrives as one `poll`, and ↵ has to run the row the
/// `not` the reader typed is showing them.
pub fn refilter(commands: &[Row], switch: &[Row], on_commands: bool, query: &str) -> Vec<Row> {
    matching(if on_commands { commands } else { switch }, query)
}

/// The box's inside, `iw` cells wide and `ih` rows tall: the mode's
/// title with the count, the filter line, the list window scrolled to
/// `first`, and the keys. In the theme in force.
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
    let line = |parts: &[(String, String)], ground: &str| -> String {
        let used: usize = parts.iter().map(|(_, t)| crate::display_width(t)).sum();
        let mut segs: Vec<(String, String)> = parts.to_vec();
        if used < iw {
            segs.push((String::new(), " ".repeat(iw - used)));
        }
        let refs: Vec<(&str, String)> = segs.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
        let body = crate::seg(&refs, iw).replace("\x1b[0m", &format!("\x1b[0m{ground}"));
        format!("{ground}{body}\x1b[0m")
    };
    let mut rows = Vec::new();
    let count = format!("{} of {} ", list.len(), total);
    let title = format!(" {mode}");
    let gap = iw.saturating_sub(crate::display_width(&title) + crate::display_width(&count)).max(1);
    rows.push(line(
        &[
            (theme::ink(Role::Accent), title),
            (String::new(), " ".repeat(gap)),
            (theme::ink(Role::Muted), count),
        ],
        &bar,
    ));
    rows.push(line(
        &[
            (theme::ink(Role::Muted), " / ".into()),
            (theme::ink(Role::Fg), format!("{query}▏")),
        ],
        &bar,
    ));
    let slots = slots(ih);
    let first = first.min(list.len().saturating_sub(slots));
    let mut listed = Vec::new();
    if list.is_empty() {
        listed.push(line(&[(theme::ink(Role::Muted), "   nothing matches".into())], &bar));
    }
    for (i, r) in list.iter().enumerate().skip(first).take(slots) {
        let here = i == selected;
        let ground = if here { theme::tint(Role::Slot) } else { bar.clone() };
        let mark = if here { "▸ " } else { "  " };
        let key = r.key.map(|k| format!("  [{k}]")).unwrap_or_default();
        listed.push(line(
            &[
                (theme::ink_on(Role::Accent, here), format!(" {mark}")),
                (theme::ink_on(Role::Fg, here), r.label.clone()),
                (theme::ink_on(Role::Muted, here), key),
                (theme::ink_on(Role::Muted, here), format!("  {}", r.detail)),
            ],
            &ground,
        ));
    }
    while listed.len() < ih.saturating_sub(3) {
        listed.push(line(&[], &bar));
    }
    rows.extend(listed);
    rows.push(line(&[(theme::ink(Role::Muted), KEYS_ROW.into())], &bar));
    rows.truncate(ih);
    rows
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
        let span = |word: &str| {
            KEYS_ROW.find(word).map(|b| {
                let at = crate::display_width(&KEYS_ROW[..b]);
                at..at + crate::display_width(word)
            })
        };
        if span("↵ run")?.contains(&inner_col) {
            return Some("enter".into());
        }
        if span("esc back")?.contains(&inner_col) {
            return Some("esc".into());
        }
        return None;
    }
    let slot = inner_row.checked_sub(2)?;
    let slots = slots(ih);
    if slot >= slots {
        return None;
    }
    let first = first.min(list.len().saturating_sub(slots));
    let at = first + slot;
    (at < list.len()).then(|| format!("pick:{at}"))
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

/// One mode's list state: what was typed, where the highlight is, and
/// where its window sits. Kept per mode, so esc back to the commands
/// finds them where they were left rather than reset.
#[derive(Default)]
struct List {
    query: String,
    selected: usize,
    first: usize,
}

/// Hand the terminal to the widget `stem` beside this one, and take it
/// back when that widget exits - the launcher's own handoff, made core
/// so the palette can start a widget from any pane. The child runs in
/// this pane; when it quits, the palette is still here, over the frame
/// it had, and esc is one press away.
fn start(keyboard: &mut crate::Keyboard, stem: &str) {
    keyboard.restore();
    crate::restore_screen();
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(stem)));
    match beside {
        Some(path) => match std::process::Command::new(&path).status() {
            Ok(_) => {}
            Err(e) => {
                crate::out(&format!("{}: {}\r\n", path.display(), e));
                crate::flush();
                std::thread::sleep(Duration::from_secs(2));
            }
        },
        None => {
            crate::out("cannot find where this binary lives\r\n");
            crate::flush();
            std::thread::sleep(Duration::from_secs(2));
        }
    }
    // The widget left the terminal however it left it, so take it back
    // rather than assuming: cbreak again, cursor away again, screen
    // clear, mouse reporting on again - the same reclaim the launcher
    // makes after a launch, without which the wheel does nothing on
    // the way back in.
    keyboard.reclaim();
    crate::claim_screen();
}

/// Run the palette over the widget's last frame until it is escaped out
/// of. Every mode redraws over that same frame, so the theme picker and
/// the widget list swap into this box in place - one surface, the way a
/// palette mode changes - never a stack of boxes.
pub(crate) fn run(keyboard: &mut crate::Keyboard) {
    if OPEN.swap(true, Ordering::AcqRel) {
        return;
    }
    // The widget's footer is under the box now. Left bound, a click on
    // where its `↵ open` was would come back as that key, where it is
    // outside the box and means esc.
    keyboard.forget_footer();
    let (base_rows, base_plots) = crate::LAST_FRAME
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_default();
    let commands = commands();
    let switch = switch_rows();
    let mut commands_list = List::default();
    let mut switch_list = List::default();
    // Which mode the box is in: the commands, or the widgets to switch
    // to. The theme picker is a mode too, but it runs as itself, over
    // the same base, and returns here when it is escaped out of.
    let mut on_commands = true;
    loop {
        let (all, mut list) = if on_commands {
            (&commands, matching(&commands, &commands_list.query))
        } else {
            (&switch, matching(&switch, &switch_list.query))
        };
        let (w, h) = crate::size();
        let m = Modal::place(w, h);
        let room = m.h.saturating_sub(2);
        let window = slots(room);
        let current = if on_commands {
            &mut commands_list
        } else {
            &mut switch_list
        };
        if current.selected >= list.len() {
            current.selected = list.len().saturating_sub(1);
        }
        current.first = current.first.min(last_flat(list.len(), window));
        let boxed = picker::framed(
            &inside(
                if on_commands { "Commands" } else { "Widgets" },
                &list,
                all.len(),
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
        crate::draw_current(&rows, w, h, &plots);
        keyboard.typing();
        let keys = keyboard.poll();
        // Whether a key moved the highlight on this pass. Only that
        // brings the window along - the wheel writes `first` and
        // nothing else, or scrolling to look at a command would drag
        // the view straight back to the highlight.
        let mut moved = false;
        for key in keys {
            // The mode can change inside the batch (Switch widget…), so
            // the state and the rows are taken again for every key
            // rather than once per poll.
            let state = if on_commands {
                &mut commands_list
            } else {
                &mut switch_list
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
                            state.selected = i;
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
                // esc pops one level: the widgets back to the commands,
                // the commands back to the widget. ctrl-k is esc here -
                // the key that opened the palette closing it.
                "esc" | "ctrl-k" => {
                    if !on_commands {
                        on_commands = true;
                        changed = true;
                    } else {
                        return close();
                    }
                }
                "enter" => match list.get(selected).map(|r| r.runs) {
                    Some(Runs::Theme) => picker::run_over(keyboard, &base_rows, &base_plots),
                    Some(Runs::Settings) => {
                        if let Some(spec) = settings::registered() {
                            // Relaunches itself by exec on a save; what
                            // returns here is a save not made.
                            settings::run_settings(keyboard, spec);
                        }
                    }
                    Some(Runs::Notices) => crate::notices::run(keyboard),
                    Some(Runs::Widgets) => {
                        on_commands = false;
                        changed = true;
                    }
                    Some(Runs::Widget) => {
                        if let Some(stem) = list.get(selected).map(|r| r.label.clone()) {
                            start(keyboard, &stem);
                        }
                    }
                    None => {}
                },
                "up" => {
                    state.selected = state.selected.saturating_sub(1);
                    moved = true;
                }
                "down" => {
                    state.selected = (state.selected + 1).min(list.len().saturating_sub(1));
                    moved = true;
                }
                "pgup" => {
                    state.selected = state.selected.saturating_sub(room.saturating_sub(3).max(1));
                    moved = true;
                }
                "pgdn" => {
                    state.selected = (state.selected + room.saturating_sub(3).max(1))
                        .min(list.len().saturating_sub(1));
                    moved = true;
                }
                "home" => {
                    state.selected = 0;
                    moved = true;
                }
                "end" => {
                    state.selected = list.len().saturating_sub(1);
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
                let state = if on_commands {
                    &mut commands_list
                } else {
                    &mut switch_list
                };
                list = refilter(&commands, &switch, on_commands, &state.query);
                state.selected = state.selected.min(list.len().saturating_sub(1));
            }
        }
        // On the pass a key moved the highlight, the window follows it.
        let current = if on_commands {
            &mut commands_list
        } else {
            &mut switch_list
        };
        if moved {
            current.first = follow_flat(current.selected, window, current.first);
        }
        current.first = current.first.min(last_flat(list.len(), window));
        keyboard.wait(Duration::from_millis(60));
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
        let rows = commands();
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
        };
        let rows = vec![
            row("Theme…", "pick a theme for this pane", None, Runs::Theme),
            row("Settings…", "this widget's settings", Some(","), Runs::Settings),
            row("Switch widget…", "start another widget in this pane", None, Runs::Widgets),
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
        let commands = commands();
        let switch = switch_rows();
        let before = refilter(&commands, &switch, true, "");
        assert_eq!(before.first().map(|r| r.label.clone()), Some("Theme…".to_string()));
        let after = refilter(&commands, &switch, true, "not");
        assert_eq!(after.first().map(|r| r.label.clone()), Some("Notices".to_string()));
        // A mode change refilters too, against the other list.
        let widgets = refilter(&commands, &switch, false, "");
        assert!(widgets.iter().all(|r| r.runs == Runs::Widget));
        assert_eq!(widgets.len(), switch.len());
    }

    #[test]
    fn the_box_fits_every_pane_and_answers_clicks() {
        let rows = commands();
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
            assert_eq!(click_key(col, m.y + 3, w, h, &rows, 0).as_deref(), Some("pick:0"));
            if slots(m.h - 2) >= 2 {
                assert_eq!(click_key(col, m.y + 4, w, h, &rows, 0).as_deref(), Some("pick:1"));
            }
            assert_eq!(click_key(0, 0, w, h, &rows, 0).as_deref(), Some("esc"));
            // The keys row answers the keys it names.
            let keys_row = m.y + m.h - 2;
            let at = |word: &str| m.x + 1 + crate::display_width(&KEYS_ROW[..KEYS_ROW.find(word).unwrap()]);
            assert_eq!(click_key(at("↵ run") + 1, keys_row, w, h, &rows, 0).as_deref(), Some("enter"));
            assert_eq!(click_key(at("esc back") + 1, keys_row, w, h, &rows, 0).as_deref(), Some("esc"));
        }
    }

    #[test]
    fn the_options_sit_one_a_row_as_the_theme_list_does() {
        let rows = commands();
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
        // Title, filter, then one option a row, with the same line height
        // as the theme list.
        assert!(plain(&inner[2]).contains("Theme…"), "{:?}", inner[2]);
        assert!(plain(&inner[3]).contains("Switch widget…"), "{:?}", inner[3]);
        assert!(plain(&inner[4]).contains("Notices"), "{:?}", inner[4]);
        assert_eq!(slots(20), 17);
        assert_eq!(slots(20), picker::slots(20), "the palette and the theme list space their rows alike");
        // Only the selected option's row is tinted.
        assert!(plain(&inner[2]).contains("▸"));
        assert!(!plain(&inner[3]).contains("▸"));
    }

    #[test]
    fn the_window_follows_the_highlight_only_as_far_as_it_must() {
        assert_eq!(follow_flat(5, 10, 2), 2);
        assert_eq!(follow_flat(20, 10, 0), 11);
        assert_eq!(follow_flat(0, 10, 8), 0);
        assert_eq!(last_flat(53, 10), 43);
    }

    #[test]
    fn an_empty_list_says_so() {
        let rows: Vec<Row> = Vec::new();
        let inner = inside("Commands", &rows, 0, 0, 0, "zz", 60, 12);
        assert!(inner.iter().any(|r| r.contains("nothing matches")));
        assert!(inner.iter().any(|r| r.contains("Commands")));
    }
}