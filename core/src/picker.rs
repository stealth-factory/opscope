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
//! A modal over the middle of the pane - a filter line and the themes
//! under it, framed, with a shadow - drawn as text, and in Kitty with its
//! frame and shadow as pixels. Typing filters them, fuzzily; ↑↓ move through them and the
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

/// Where the modal sits in a pane `w` by `h`: its outer box, border
/// included, centred, clear of the title row, and never wider or taller
/// than the pane can give.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Modal {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

impl Modal {
    pub fn place(w: usize, h: usize) -> Modal {
        let usable = w.saturating_sub(1);
        let mw = usable.saturating_sub(4).clamp(usable.min(28), 76);
        let mh = h.saturating_sub(3).clamp(h.saturating_sub(1).min(7), 20);
        let x = (usable.saturating_sub(mw)) / 2;
        let y = (1 + h.saturating_sub(1).saturating_sub(mh) / 2).min(h.saturating_sub(mh));
        Modal { x, y, w: mw.max(3), h: mh.max(3) }
    }

    /// Room for a shadow to its right and under it.
    fn shadowed(&self, w: usize, h: usize) -> bool {
        self.x + self.w < w.saturating_sub(1) && self.y + self.h < h
    }
}

/// The modal's inside, `iw` cells wide and `ih` rows tall: a title with the
/// count, the filter line, the list, and the keys - in the theme in force,
/// which is the one being previewed.
pub fn inside(list: &[Choice], selected: usize, query: &str, kept: &str, said: &str, iw: usize, ih: usize) -> Vec<String> {
    let bar = theme::tint(Role::Dark);
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
    let count = format!("{} of {} ", list.len(), choices().len());
    let title = " Theme";
    let gap = iw.saturating_sub(crate::display_width(title) + crate::display_width(&count)).max(1);
    rows.push(line(
        &[
            (theme::ink(Role::Accent), title.into()),
            (String::new(), " ".repeat(gap)),
            (theme::ink(Role::Muted), count),
        ],
        &bar,
    ));
    let typed = if said.is_empty() {
        vec![(theme::ink(Role::Muted), " / ".into()), (theme::ink(Role::Fg), format!("{query}▏"))]
    } else {
        vec![(theme::ink(Role::Red), format!(" {said}"))]
    };
    rows.push(line(&typed, &bar));
    let slots = ih.saturating_sub(3).max(1);
    let first = selected.saturating_sub(slots.saturating_sub(1));
    let mut listed = Vec::new();
    if list.is_empty() {
        listed.push(line(&[(theme::ink(Role::Muted), "   no theme matches".into())], &bar));
    }
    for (i, c) in list.iter().enumerate().skip(first).take(slots) {
        let here = i == selected;
        let ground = if here { theme::tint(Role::Slot) } else { bar.clone() };
        let mark = if here { "▸ " } else { "  " };
        let kept_mark = if c.value == kept { " ✓" } else { "" };
        listed.push(line(
            &[
                (theme::ink_on(Role::Accent, here), format!(" {mark}")),
                (theme::ink_on(Role::Fg, here), c.label.clone()),
                (theme::ink_on(Role::Green, here), kept_mark.into()),
                (theme::ink_on(Role::Muted, here), format!("  {}", c.detail)),
            ],
            &ground,
        ));
    }
    while listed.len() < slots {
        listed.push(line(&[], &bar));
    }
    rows.extend(listed);
    rows.push(line(&[(theme::ink(Role::Muted), " ↑↓ preview · ↵ keep · esc cancel".into())], &bar));
    rows.truncate(ih);
    rows
}

/// The modal's rows, border included: `inside` framed in rounded corners in
/// the theme's accent. This is the whole picture in a text terminal; Kitty
/// draws the frame and a shadow over it as pixels too (`chrome`).
pub fn framed(inner: &[String], iw: usize) -> Vec<String> {
    let edge = format!("{}{}", theme::ink(Role::Accent), theme::tint(Role::Dark));
    let mut rows = vec![format!("{edge}╭{}╮\x1b[0m", "─".repeat(iw))];
    for row in inner {
        rows.push(format!("{edge}│\x1b[0m{row}{edge}│\x1b[0m"));
    }
    rows.push(format!("{edge}╰{}╯\x1b[0m", "─".repeat(iw)));
    rows
}

/// The frame and the shadow as filled plots: thin pixel lines and a soft
/// shadow in Kitty, and in a text terminal the same box-drawing characters
/// `framed` already put there, with a half-block shadow.
pub fn chrome(m: Modal, w: usize, h: usize) -> Vec<crate::Plot> {
    use crate::Plot;
    let edge = format!("{}{}", theme::ink(Role::Accent), theme::tint(Role::Dark));
    let accent = theme::ink(Role::Accent);
    let dark = theme::tint(Role::Dark);
    let t = 0.12;
    let half = 0.5 - t / 2.0;
    let inner = m.w.saturating_sub(2);
    let mut plots = Vec::new();
    let cell_bg = |len_w: f64, len_h: f64| Plot::rect(0.0, 0.0, len_w, len_h, &dark);
    // Top: the line from the middle of the first cell to the middle of the
    // last, and the two corners turning down.
    plots.push(Plot::filled(
        m.x,
        m.y,
        m.w,
        1,
        vec![
            cell_bg(m.w as f64, 1.0),
            Plot::rect(0.5 - t / 2.0, half, m.w as f64 - 1.0 + t, t, &accent),
            Plot::rect(half, half, t, 1.0 - half, &accent),
            Plot::rect(m.w as f64 - 0.5 - t / 2.0, half, t, 1.0 - half, &accent),
        ],
        vec![vec![(edge.clone(), format!("╭{}╮", "─".repeat(inner)))]],
    ));
    plots.push(Plot::filled(
        m.x,
        m.y + m.h - 1,
        m.w,
        1,
        vec![
            cell_bg(m.w as f64, 1.0),
            Plot::rect(0.5 - t / 2.0, half, m.w as f64 - 1.0 + t, t, &accent),
            Plot::rect(half, 0.0, t, half + t, &accent),
            Plot::rect(m.w as f64 - 0.5 - t / 2.0, 0.0, t, half + t, &accent),
        ],
        vec![vec![(edge.clone(), format!("╰{}╯", "─".repeat(inner)))]],
    ));
    let sides = m.h.saturating_sub(2);
    if sides > 0 {
        for at in [m.x, m.x + m.w - 1] {
            plots.push(Plot::filled(
                at,
                m.y + 1,
                1,
                sides,
                vec![cell_bg(1.0, sides as f64), Plot::rect(half, 0.0, t, sides as f64, &accent)],
                (0..sides).map(|_| vec![(edge.clone(), "│".to_string())]).collect(),
            ));
        }
    }
    if m.shadowed(w, h) {
        let ground = theme::current();
        let bg = ground.colour(Role::Bg);
        let (r, g, b) = theme::mix(bg, (0, 0, 0), if ground.light { 0.22 } else { 0.6 });
        let shade = format!("\x1b[38;2;{r};{g};{b}m");
        plots.push(Plot::filled(
            m.x + m.w,
            m.y + 1,
            1,
            m.h,
            vec![Plot::rect(0.0, 0.0, 0.5, m.h as f64 - 0.5, &shade)],
            (0..m.h).map(|i| vec![(shade.clone(), if i + 1 == m.h { "▘" } else { "▌" }.to_string())]).collect(),
        ));
        // Stops short of the corner, which is the column's `▘`.
        plots.push(Plot::filled(
            m.x + 1,
            m.y + m.h,
            m.w - 1,
            1,
            vec![Plot::rect(0.0, 0.0, m.w as f64 - 1.0, 0.5, &shade)],
            vec![vec![(shade.clone(), "▀".repeat(m.w - 1))]],
        ));
    }
    plots
}

/// The columns `from..to` of a coloured row, its colours carried in: the
/// colour in force at `from` is set again at the start, and a character two
/// cells wide that a cut lands in the middle of is drawn as spaces.
pub fn cut(row: &str, from: usize, to: usize) -> String {
    let mut out = String::new();
    let mut style = String::new();
    let mut col = 0usize;
    let mut started = false;
    let chars: Vec<char> = row.chars().collect();
    let mut i = 0;
    while i < chars.len() && col < to {
        let c = chars[i];
        if c == '\x1b' {
            let mut j = i + 1;
            let csi = chars.get(j) == Some(&'[');
            if csi {
                j += 1;
                while j < chars.len() && !('@'..='~').contains(&chars[j]) {
                    j += 1;
                }
            } else if chars.get(j) == Some(&']') {
                while j < chars.len() && chars[j] != '\x07' && !(chars[j] == '\\' && chars[j - 1] == '\x1b') {
                    j += 1;
                }
            }
            let seq: String = chars[i..=j.min(chars.len() - 1)].iter().collect();
            if csi && seq.ends_with('m') {
                if seq == "\x1b[0m" || seq == "\x1b[m" {
                    style.clear();
                } else {
                    style.push_str(&seq);
                }
            }
            if started {
                out.push_str(&seq);
            }
            i = j + 1;
            continue;
        }
        let cw = crate::display_width(&c.to_string());
        if col + cw > from && !started {
            out.push_str(&style);
            started = true;
        }
        if col >= from && col + cw <= to {
            out.push(c);
        } else if col < from && col + cw > from {
            out.push_str(&" ".repeat((col + cw).min(to) - from));
        } else if col >= from && col + cw > to {
            out.push_str(&" ".repeat(to - col));
        }
        col += cw;
        i += 1;
    }
    if started {
        out.push_str("\x1b[0m");
    }
    out
}

/// `base` with `piece`, `width` cells wide, laid over it at column `x`:
/// the widget's row either side of the modal kept as it was.
pub fn splice(base: &str, x: usize, piece: &str, width: usize, w: usize) -> String {
    let left = cut(base, 0, x);
    let pad = x.saturating_sub(crate::display_width(&left));
    let right = cut(base, x + width, w.saturating_sub(1));
    format!("{left}{}{piece}{right}", " ".repeat(pad))
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
        let m = Modal::place(w, h);
        let room = m.h.saturating_sub(2);
        let boxed = framed(&inside(&list, selected, &query, &kept, &said, m.w.saturating_sub(2), room), m.w.saturating_sub(2));
        let mut rows: Vec<String> = base_rows.iter().take(h).cloned().collect();
        rows.resize(h, String::new());
        for (i, row) in boxed.iter().enumerate() {
            if let Some(base) = rows.get_mut(m.y + i) {
                *base = splice(base, m.x, row, m.w, w);
            }
        }
        // The widget's own charts stay where the modal and its shadow are
        // not; one under the modal would be drawn over it.
        let (bx, by, bw, bh) = (m.x, m.y, m.w + 1, m.h + 1);
        let mut plots: Vec<crate::Plot> = base_plots
            .iter()
            .filter(|p| p.x >= bx + bw || p.x + p.width <= bx || p.y >= by + bh || p.y + p.height <= by)
            .cloned()
            .collect();
        plots.extend(chrome(m, w, h));
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
                "pgup" => selected = selected.saturating_sub(room.saturating_sub(3).max(1)),
                "pgdn" => selected = (selected + room.saturating_sub(3).max(1)).min(list.len().saturating_sub(1)),
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
    fn the_modal_fits_every_pane_and_marks_the_saved_theme() {
        let all = choices();
        for (w, h) in [(30, 10), (40, 12), (80, 24), (140, 50), (24, 8)] {
            let m = Modal::place(w, h);
            assert!(m.x + m.w < w, "{w}x{h}: {m:?}");
            assert!(m.y >= 1 && m.y + m.h <= h, "{w}x{h}: {m:?} covers the title or runs off");
            let inner = inside(&all, 3, "do", "dusk", "", m.w - 2, m.h - 2);
            assert_eq!(inner.len(), m.h - 2, "{w}x{h}");
            for r in &inner {
                assert_eq!(crate::display_width(r), m.w - 2, "{w}x{h}: {r:?}");
            }
            for r in framed(&inner, m.w - 2) {
                assert_eq!(crate::display_width(&r), m.w, "{w}x{h}");
            }
            for p in chrome(m, w, h) {
                assert!(p.x + p.width < w + 1 && p.y + p.height <= h, "{w}x{h}: a chrome plot off the pane");
            }
        }
        let rows = inside(&all, 2, "", "dusk", "", 60, 12);
        assert!(rows.iter().any(|r| r.contains("opscope Dusk") && r.contains('✓')));
        assert!(rows[0].contains("Theme") && rows[0].contains("of 53"));
        assert!(inside(&[], 0, "zz", "dusk", "", 40, 8).iter().any(|r| r.contains("no theme matches")));
    }

    #[test]
    fn in_text_the_frame_and_shadow_are_drawn_as_characters() {
        let (w, h) = (100, 30);
        let m = Modal::place(w, h);
        assert!(m.shadowed(w, h), "{m:?}");
        let rows = vec!["x".repeat(w - 1); h];
        let (out, _, _) = crate::graphics::compose(&rows, &chrome(m, w, h), w, h, false, false, false, false);
        let all = out.join("\n");
        for c in ["╭", "╯", "▌", "▘", "▀"] {
            assert!(all.contains(c), "{c} missing from the text frame");
        }
    }

    #[test]
    fn a_row_is_cut_and_spliced_by_cells_with_its_colours() {
        let red = "\x1b[38;2;255;0;0m";
        let row = format!("ab{red}cdef\x1b[0mgh");
        // The colour in force at the cut is carried into the piece.
        assert_eq!(cut(&row, 3, 5), format!("{red}de\x1b[0m"));
        assert_eq!(cut(&row, 0, 2), "ab\x1b[0m");
        let spliced = splice(&row, 2, "XY", 2, 9);
        let plain: String = {
            let mut out = String::new();
            let mut skip = false;
            for c in spliced.chars() {
                if c == '\x1b' { skip = true; continue; }
                if skip { if c == 'm' { skip = false; } continue; }
                out.push(c);
            }
            out
        };
        assert_eq!(plain, "abXYefgh");
        // A wide character a cut lands inside becomes spaces, never half a glyph.
        assert_eq!(crate::display_width(&cut("月月月", 1, 4)), 3);
        // A row shorter than the cut is padded so the piece lands in place.
        assert!(splice("ab", 5, "X", 1, 20).starts_with("ab\x1b[0m   X") || splice("ab", 5, "X", 1, 20).contains("   X"));
    }
}
