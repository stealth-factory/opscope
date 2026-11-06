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

//! The theme picker: `ctrl-t` in any widget.
//!
//! A box over the bottom of the pane with a filter line and the themes
//! under it. Typing filters them, fuzzily; ↑↓ move through them and the
//! widget behind repaints in the one under the cursor straight away - its
//! last frame, drawn in that theme - so a theme is seen in place before it
//! is kept. ↵ keeps it: `opscope.theme` in the main config file, which
//! every other running widget picks up within two seconds. esc puts back
//! the theme there was.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::theme::{self, Role, Theme, Wanted};

/// Open, so the key that opens it is not answered again inside it, and so
/// its own frames are not taken for the widget's.
pub(crate) static OPEN: AtomicBool = AtomicBool::new(false);

/// The key that opens it, and the hint every footer carries for it.
pub const KEY: &str = "ctrl-t";
/// `ctrl-t` leads the hint so a click on it is that key, as `ctrl-y`
/// is in the footers that name it.
pub const HINT: &str = "ctrl-t theme";

/// One thing the picker offers: a setting value, what to call it, and the
/// words it is matched against.
#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub value: String,
    pub label: String,
    pub detail: String,
}

/// Everything there is to pick: auto, the terminal's own, then every theme
/// in the picker's group order.
pub fn choices() -> Vec<Choice> {
    let groups = theme::groups();
    let mut out = vec![
        Choice {
            value: "auto".into(),
            label: "auto".into(),
            detail: "the terminal's own colours, else opscope Dusk".into(),
        },
        Choice {
            value: "terminal".into(),
            label: "terminal".into(),
            detail: "always the terminal's own colours".into(),
        },
    ];
    let all = theme::all();
    for (group, label) in &groups {
        for t in all.iter().filter(|t| &t.group == group) {
            let mut detail = label.clone();
            if let Some(film) = &t.film {
                detail = format!("{label} · {film}");
            }
            out.push(Choice { value: t.id.clone(), label: t.name.clone(), detail });
        }
    }
    out
}

/// How well `query` matches `text`, lower is better: every character of
/// the query in order, case-insensitive, with gaps and a late start costing
/// and a match at the start of a word costing less. `None` when it does not
/// match at all. An empty query matches everything equally.
pub fn fuzzy(query: &str, text: &str) -> Option<i32> {
    let q: Vec<char> = query.to_lowercase().chars().filter(|c| !c.is_whitespace()).collect();
    if q.is_empty() {
        return Some(0);
    }
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let wanted = q.len();
    let mut score = 0i32;
    let mut at = 0usize;
    let mut loose = 0usize;
    let mut last: Option<usize> = None;
    for qc in q {
        let found = (at..t.len()).find(|&i| t[i] == qc)?;
        let gap = match last {
            Some(prev) => (found - prev - 1) as i32,
            None => found as i32,
        };
        let word_start = found == 0 || !t[found - 1].is_alphanumeric();
        score += gap * 2 - if word_start { 3 } else { 0 };
        if !word_start {
            loose += gap.max(0) as usize;
        }
        last = Some(found);
        at = found + 1;
    }
    // Letters scattered through the middle of words are not a match anybody
    // meant: "drac" must not find d-r-a-c spread through "editor classics".
    // A jump to the start of a word is free - "tn" is Tokyo Night.
    if loose > wanted {
        return None;
    }
    Some(score)
}

/// The choices that match `query`, best first; in their own order when
/// the query is empty.
pub fn filtered(all: &[Choice], query: &str) -> Vec<Choice> {
    let mut scored: Vec<(i32, usize, &Choice)> = all
        .iter()
        .enumerate()
        .filter_map(|(i, c)| {
            // Each field on its own, so a query never matches across them;
            // the name and id before the group and film words.
            let best = [fuzzy(query, &c.label), fuzzy(query, &c.value), fuzzy(query, &c.detail).map(|s| s + 20)]
                .into_iter()
                .flatten()
                .min()?;
            Some((best, i, c))
        })
        .collect();
    scored.sort_by_key(|(s, i, _)| (*s, *i));
    scored.into_iter().map(|(_, _, c)| c.clone()).collect()
}

/// The theme a choice draws in, for the preview.
fn preview_of(value: &str) -> Theme {
    let wanted = theme::parse_wanted(value);
    let terminal = match wanted {
        Wanted::Named(_) => None,
        _ => crate::terminal::terminal_theme(),
    };
    theme::choose(&wanted, terminal.as_ref())
        .unwrap_or_else(|_| theme::choose(&Wanted::Auto, terminal.as_ref()).expect("auto always has a theme"))
}

/// What is saved now, as the picker marks it.
fn saved() -> String {
    match theme::wanted() {
        Wanted::Auto => "auto".into(),
        Wanted::Terminal => "terminal".into(),
        Wanted::Named(id) => id,
    }
}

/// The box drawn over the bottom of the pane: its rows, top to bottom, in
/// the theme in force (the one being previewed).
pub fn overlay(list: &[Choice], selected: usize, query: &str, kept: &str, said: &str, w: usize, room: usize) -> Vec<String> {
    let width = w.saturating_sub(1).max(1);
    let bar = theme::tint(Role::Dark);
    let line = |parts: &[(String, String)], ground: &str| -> String {
        let used: usize = parts.iter().map(|(_, t)| crate::display_width(t)).sum();
        let mut segs: Vec<(String, String)> = parts.to_vec();
        if used < width {
            segs.push((String::new(), " ".repeat(width - used)));
        }
        let refs: Vec<(&str, String)> = segs.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
        format!("{ground}{}\x1b[0m", crate::seg(&refs, width).replace("\x1b[0m", &format!("\x1b[0m{ground}")))
    };
    let mut rows = Vec::new();
    rows.push(line(
        &[
            (theme::ink(Role::Accent), " ── THEME ── ".into()),
            (theme::ink(Role::Muted), "type to filter · ↑↓ preview · ↵ keep · esc cancel".into()),
        ],
        &bar,
    ));
    let count = format!("   {} of {}", list.len(), choices().len());
    let note = if said.is_empty() { count } else { format!("   {said}") };
    rows.push(line(
        &[
            (theme::ink(Role::Muted), " / ".into()),
            (theme::ink(Role::Fg), format!("{query}▏")),
            (theme::ink(if said.is_empty() { Role::Muted } else { Role::Red }), note),
        ],
        &bar,
    ));
    let slots = room.saturating_sub(rows.len()).max(1);
    let first = selected.saturating_sub(slots.saturating_sub(1));
    if list.is_empty() {
        rows.push(line(&[(theme::ink(Role::Muted), "   no theme matches".into())], &bar));
    }
    for (i, c) in list.iter().enumerate().skip(first).take(slots) {
        let here = i == selected;
        let ground = if here { theme::tint(Role::Slot) } else { bar.clone() };
        let mark = if here { "▸ " } else { "  " };
        let kept_mark = if c.value == kept { " ✓" } else { "" };
        rows.push(line(
            &[
                (theme::ink_on(Role::Accent, here), format!(" {mark}")),
                (theme::ink_on(Role::Fg, here), c.label.clone()),
                (theme::ink_on(Role::Green, here), kept_mark.into()),
                (theme::ink_on(Role::Muted, here), format!("   {}", c.detail)),
            ],
            &ground,
        ));
    }
    rows
}

/// Run the picker over the widget's last frame until ↵ or esc.
pub(crate) fn run(keyboard: &mut crate::Keyboard) {
    if OPEN.swap(true, Ordering::AcqRel) {
        return;
    }
    let original = theme::current();
    let (base_rows, base_plots) = crate::LAST_FRAME
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_default();
    let all = choices();
    let kept = saved();
    let mut query = String::new();
    let mut list = filtered(&all, &query);
    let mut selected = list.iter().position(|c| c.value == kept).unwrap_or(0);
    let mut shown: Option<String> = None;
    let mut said = String::new();
    loop {
        if let Some(c) = list.get(selected) {
            if shown.as_deref() != Some(c.value.as_str()) {
                theme::set(preview_of(&c.value));
                shown = Some(c.value.clone());
            }
        }
        let (w, h) = crate::size();
        let room = (h * 2 / 3).clamp(4, 16).min(h.saturating_sub(1).max(1));
        let boxed = overlay(&list, selected, &query, &kept, &said, w, room);
        let mut rows: Vec<String> = base_rows.iter().take(h).cloned().collect();
        rows.resize(h, String::new());
        let start = h.saturating_sub(boxed.len());
        for (i, row) in boxed.into_iter().enumerate() {
            if start + i < rows.len() {
                rows[start + i] = row;
            }
        }
        let plots: Vec<crate::Plot> = base_plots
            .iter()
            .filter(|p| p.y + p.height <= start)
            .cloned()
            .collect();
        crate::draw_plots(&rows, w, h, &plots);
        keyboard.typing();
        let keys = keyboard.poll();
        for key in keys {
            match key.as_str() {
                "esc" | "ctrl-t" => {
                    theme::set((*original).clone());
                    return close();
                }
                "enter" => {
                    if let Some(c) = list.get(selected) {
                        match theme::save(&c.value) {
                            Ok(()) => return close(),
                            Err(why) => said = format!("could not save: {why}"),
                        }
                    }
                }
                "up" => selected = selected.saturating_sub(1),
                "down" => selected = (selected + 1).min(list.len().saturating_sub(1)),
                "pgup" => selected = selected.saturating_sub(room.max(1)),
                "pgdn" => selected = (selected + room.max(1)).min(list.len().saturating_sub(1)),
                "home" => selected = 0,
                "end" => selected = list.len().saturating_sub(1),
                "backspace" => {
                    query.pop();
                    list = filtered(&all, &query);
                    selected = 0;
                }
                other if other.chars().count() == 1 && !other.chars().all(char::is_control) => {
                    query.push_str(other);
                    list = filtered(&all, &query);
                    selected = 0;
                }
                _ => {}
            }
        }
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
    fn fuzzy_matches_in_order_and_prefers_word_starts() {
        assert!(fuzzy("", "anything").is_some());
        assert!(fuzzy("gl", "GitHub Light").is_some());
        assert!(fuzzy("tg", "GitHub").is_none(), "order matters: g only comes before t");
        assert!(fuzzy("zz", "Dracula").is_none());
        // The start of a word beats the middle of one.
        assert!(fuzzy("n", "Nord").unwrap() < fuzzy("n", "Dusk night").unwrap() || fuzzy("n", "Nord").unwrap() <= 0);
        assert!(fuzzy("tn", "Tokyo Night").unwrap() < fuzzy("tn", "Catppuccin").unwrap_or(i32::MAX));
        assert!(fuzzy("drac", "editor classics").is_none(), "scattered letters are no match");
    }

    #[test]
    fn every_theme_is_offered_after_auto_and_terminal() {
        let all = choices();
        assert_eq!(all[0].value, "auto");
        assert_eq!(all[1].value, "terminal");
        assert_eq!(all.len(), 2 + theme::all().len());
        assert!(all.iter().any(|c| c.value == "dusk" && c.label == "opscope Dusk"));
        assert!(all.iter().any(|c| c.value == "digital-rain" && c.detail.contains("The Matrix")));
    }

    #[test]
    fn filtering_finds_by_name_id_or_group() {
        let all = choices();
        let light = filtered(&all, "light");
        assert!(!light.is_empty() && light.iter().any(|c| c.value == "github-light"));
        let drac = filtered(&all, "drac");
        assert_eq!(drac[0].value, "dracula");
        assert_eq!(drac.len(), 1, "only Dracula: {:?}", drac.iter().map(|c| &c.value).collect::<Vec<_>>());
        // A group word still finds the whole group, after any name match.
        let pastel = filtered(&all, "pastel");
        assert!(pastel.len() >= 5 && pastel.iter().all(|c| c.detail.contains("pastel")));
        assert!(filtered(&all, "qqqqzz").is_empty());
        assert_eq!(filtered(&all, "").len(), all.len());
    }

    #[test]
    fn the_box_fits_the_pane_and_marks_the_saved_theme() {
        let all = choices();
        for w in [24, 40, 80, 140] {
            let rows = overlay(&all, 3, "do", "dusk", "", w, 10);
            assert!(rows.len() <= 10);
            for r in &rows {
                assert!(crate::display_width(r) < w, "{w}: {r:?}");
            }
        }
        let rows = overlay(&all, 2, "", "dusk", "", 100, 8);
        assert!(rows.iter().any(|r| r.contains("opscope Dusk") && r.contains('✓')));
        assert!(overlay(&[], 0, "zz", "dusk", "", 80, 6).iter().any(|r| r.contains("no theme matches")));
    }
}
