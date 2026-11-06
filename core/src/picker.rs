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
/// In the footer's own form, `[q]uit` - the key in brackets, fused onto
/// the word - and a click on it is `ctrl-t`.
pub const HINT: &str = "[ctrl-t]heme";

/// One thing the picker offers: a setting value, what to call it, and the
/// words it is matched against.
#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub value: String,
    pub label: String,
    pub detail: String,
    pub kind: Kind,
}

/// The picker's three sections, in the order they are listed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// `auto` and `terminal`: settings that follow the terminal rather than
    /// themes of their own.
    Setting,
    /// A file in the user's themes folder.
    Yours,
    /// One of the themes in the binary.
    Bundled,
}

/// One row of the list as drawn.
#[derive(Clone, Debug, PartialEq)]
pub enum Row {
    /// A theme or setting: its index in the list.
    Item(usize),
    /// A section's heading rule, with its label.
    Rule(String),
}

/// The themes folder as someone would type it: under `~` when it is.
pub fn folder() -> String {
    let dir = theme::themes_dir().display().to_string();
    match std::env::var("HOME").ok().filter(|h| !h.is_empty()) {
        Some(home) if dir.starts_with(&home) => format!("~{}", &dir[home.len()..]),
        _ => dir,
    }
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
            kind: Kind::Setting,
        },
        Choice {
            value: "terminal".into(),
            label: "terminal".into(),
            detail: "always the terminal's own colours".into(),
            kind: Kind::Setting,
        },
    ];
    let all = theme::all();
    // Yours first, named by the file each came from, then the bundled
    // themes group by group.
    for t in all.iter().filter(|t| t.group == "user") {
        out.push(Choice { value: t.id.clone(), label: t.name.clone(), detail: format!("{}.json", t.id), kind: Kind::Yours });
    }
    for (group, label) in groups.iter().filter(|(g, _)| g != "user") {
        for t in all.iter().filter(|t| &t.group == group) {
            let mut detail = label.clone();
            if let Some(film) = &t.film {
                detail = format!("{label} · {film}");
            }
            out.push(Choice { value: t.id.clone(), label: t.name.clone(), detail, kind: Kind::Bundled });
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
/// The modal's own background: the theme's `dark`, or none under NO_COLOR,
/// where every background is reverse video and a modal drawn in one would
/// leave the highlighted row nothing to stand out from.
fn modal_ground() -> String {
    if theme::no_color() {
        String::new()
    } else {
        theme::tint(Role::Dark)
    }
}

/// The modal's last row. A click on `↵ keep` or `esc cancel` is that key.
const KEYS_ROW: &str = " ↑↓ preview · ↵ keep · esc cancel";

/// What a click at `col`, `row` of a pane `w` by `h` means while the picker
/// shows `list` scrolled to `first`: the key it stands for. On a theme it
/// is `pick:<index>`, which `run` turns into a move or, on the theme already
/// highlighted, into `enter` - the same as a click on a widget's row.
pub fn click_key(col: usize, row: usize, w: usize, h: usize, list: &[Choice], first: usize) -> Option<String> {
    let m = Modal::place(w, h);
    if col < m.x || col >= m.x + m.w || row < m.y || row >= m.y + m.h {
        return Some("esc".into());
    }
    let ih = m.h.saturating_sub(2);
    let inner_row = row.checked_sub(m.y + 1)?;
    let inner_col = col.checked_sub(m.x + 1)?;
    if inner_row >= ih {
        return None;
    }
    if inner_row + 1 == ih {
        let span = |word: &str| {
            KEYS_ROW.find(word).map(|b| {
                let at = crate::display_width(&KEYS_ROW[..b]);
                at..at + crate::display_width(word)
            })
        };
        if span("↵ keep")?.contains(&inner_col) {
            return Some("enter".into());
        }
        if span("esc cancel")?.contains(&inner_col) {
            return Some("esc".into());
        }
        return None;
    }
    let slot = inner_row.checked_sub(2)?;
    if slot >= slots(ih) {
        return None;
    }
    let drawn = drawn(list);
    let first = first.min(drawn.len().saturating_sub(slots(ih)));
    match drawn.get(first + slot)? {
        Row::Item(index) => Some(format!("pick:{index}")),
        _ => None,
    }
}

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
/// How many rows of the list the modal's inside shows, `ih` rows tall: all
/// but the title, the filter line and the keys.
pub fn slots(ih: usize) -> usize {
    ih.saturating_sub(3).max(1)
}

/// The list as drawn, one entry per row: `auto` and `terminal`, your
/// themes if there are any, then the bundled ones. A section after the
/// first opens with a rule naming it, so a setting reads as a different
/// kind of choice from a theme, and your themes as yours - with the folder
/// they were read from.
pub fn drawn(list: &[Choice]) -> Vec<Row> {
    drawn_with(list, &folder())
}

fn drawn_with(list: &[Choice], folder: &str) -> Vec<Row> {
    let mut out = Vec::new();
    let mut last: Option<Kind> = None;
    for (i, c) in list.iter().enumerate() {
        if last.is_some_and(|k| k != c.kind) {
            out.push(match c.kind {
                Kind::Yours => Row::Rule(format!("your themes · {folder}")),
                Kind::Bundled => Row::Rule("bundled".into()),
                Kind::Setting => Row::Rule(String::new()),
            });
        }
        out.push(Row::Item(i));
        last = Some(c.kind);
    }
    out
}

/// The first drawn row of a window `slots` tall that keeps `selected` in
/// view, moving from `first` no further than it has to.
pub fn follow_list(list: &[Choice], selected: usize, slots: usize, first: usize) -> usize {
    let at = drawn(list).iter().position(|d| *d == Row::Item(selected)).unwrap_or(0);
    let slots = slots.max(1);
    if at < first {
        at
    } else if at >= first + slots {
        at + 1 - slots
    } else {
        first
    }
}

/// The furthest the list can be scrolled: its last row on the window's last.
pub fn last_first(list: &[Choice], slots: usize) -> usize {
    drawn(list).len().saturating_sub(slots.max(1))
}

/// `inside_from` with the window wherever keeps `selected` in view.
#[cfg(test)]
pub fn inside(list: &[Choice], selected: usize, query: &str, kept: &str, said: &str, iw: usize, ih: usize) -> Vec<String> {
    let first = follow_list(list, selected, slots(ih), 0);
    inside_from(list, selected, first, query, kept, said, iw, ih)
}

/// The modal's inside with its list scrolled to drawn row `first`.
#[allow(clippy::too_many_arguments)]
pub fn inside_from(
    list: &[Choice],
    selected: usize,
    first: usize,
    query: &str,
    kept: &str,
    said: &str,
    iw: usize,
    ih: usize,
) -> Vec<String> {
    let bar = modal_ground();
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
    // Under NO_COLOR a theme is still saved, for the panes where it isn't
    // set; this one stays uncoloured whatever is picked.
    let title = if theme::no_color() { " Theme · NO_COLOR is set here" } else { " Theme" };
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
    let slots = slots(ih);
    let drawn = drawn(list);
    let first = first.min(drawn.len().saturating_sub(slots));
    let mut listed = Vec::new();
    if list.is_empty() {
        listed.push(line(&[(theme::ink(Role::Muted), "   no theme matches".into())], &bar));
    }
    for d in drawn.iter().skip(first).take(slots) {
        let i = match d {
            Row::Item(i) => *i,
            Row::Rule(label) => {
                // ` ── label ───…`, the rule to the modal's edge.
                let head = if label.is_empty() { String::new() } else { format!("{label} ") };
                let used = 4 + crate::display_width(&head);
                listed.push(line(
                    &[
                        (theme::ink(Role::Muted), " ── ".into()),
                        (theme::ink(Role::Muted), head),
                        (theme::ink(Role::Muted), "─".repeat(iw.saturating_sub(used + 1))),
                    ],
                    &bar,
                ));
                continue;
            }
        };
        let c = &list[i];
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
    rows.push(line(&[(theme::ink(Role::Muted), KEYS_ROW.into())], &bar));
    rows.truncate(ih);
    rows
}

/// The modal's rows, border included: `inside` framed in rounded corners in
/// the theme's accent. This is the whole picture in a text terminal; Kitty
/// draws the frame and a shadow over it as pixels too (`chrome`).
pub fn framed(inner: &[String], iw: usize) -> Vec<String> {
    let edge = format!("{}{}", theme::ink(Role::Accent), modal_ground());
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
    let edge = format!("{}{}", theme::ink(Role::Accent), modal_ground());
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
    // Where the list's window starts. The wheel moves it and nothing else;
    // a key that moves the highlight brings it along, on that frame only.
    let mut first = 0usize;
    let mut moved = true;
    loop {
        if let Some(c) = list.get(selected) {
            // No preview under NO_COLOR: this pane draws no theme's colours,
            // and a theme that paints its background would come out as a
            // whole screen in reverse video.
            if shown.as_deref() != Some(c.value.as_str()) && !theme::no_color() {
                theme::set(preview_of(&c.value));
                shown = Some(c.value.clone());
            }
        }
        let (w, h) = crate::size();
        let m = Modal::place(w, h);
        let room = m.h.saturating_sub(2);
        let window = slots(room);
        if moved {
            first = follow_list(&list, selected, window, first);
            moved = false;
        }
        first = first.min(last_first(&list, window));
        let boxed = framed(
            &inside_from(&list, selected, first, &query, &kept, &said, m.w.saturating_sub(2), room),
            m.w.saturating_sub(2),
        );
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
            let before = (selected, query.clone());
            // A click is the key it stands for: outside the modal it is
            // esc, on the keys row the key it names, and on a theme a move
            // there - or ↵, when that theme is already the one shown.
            let key = match crate::click_at(&key) {
                Some((col, row)) => match click_key(col, row, w, h, &list, first) {
                    Some(k) => match k.strip_prefix("pick:").and_then(|i| i.parse::<usize>().ok()) {
                        Some(i) if i == selected => "enter".to_string(),
                        Some(i) => {
                            // Already in view, so the window stays put.
                            selected = i;
                            continue;
                        }
                        None => k,
                    },
                    None => continue,
                },
                None => key,
            };
            match key.as_str() {
                // The wheel and ctrl-y/ctrl-e move the view, never the
                // highlight, so scrolling never changes the preview.
                "wheel-up" => first = first.saturating_sub(3),
                "wheel-down" => first = (first + 3).min(last_first(&list, window)),
                "ctrl-y" => first = first.saturating_sub(1),
                "ctrl-e" => first = (first + 1).min(last_first(&list, window)),
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
            if (selected, query.clone()) != before {
                moved = true;
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
    fn a_click_is_the_key_it_stands_for() {
        let all = choices();
        let (w, h) = (100, 30);
        let m = Modal::place(w, h);
        let ih = m.h - 2;
        let col = m.x + 5;
        // The first list row is auto, the third the rule, the fourth Dusk.
        assert_eq!(click_key(col, m.y + 3, w, h, &all, 0).as_deref(), Some("pick:0"));
        assert_eq!(click_key(col, m.y + 5, w, h, &all, 0), None);
        assert_eq!(click_key(col, m.y + 6, w, h, &all, 0).as_deref(), Some("pick:2"));
        // Scrolled, the same row is a later theme.
        assert_eq!(click_key(col, m.y + 3, w, h, &all, 4).as_deref(), Some("pick:3"));
        // The keys row: each hint is its key, the arrows nothing.
        let keys_row = m.y + ih;
        let at = |word: &str| m.x + 1 + crate::display_width(&KEYS_ROW[..KEYS_ROW.find(word).unwrap()]);
        assert_eq!(click_key(at("↵ keep") + 1, keys_row, w, h, &all, 0).as_deref(), Some("enter"));
        assert_eq!(click_key(at("esc cancel") + 2, keys_row, w, h, &all, 0).as_deref(), Some("esc"));
        assert_eq!(click_key(at("↑↓"), keys_row, w, h, &all, 0), None);
        // The title, the filter line and the frame are nothing; outside is esc.
        assert_eq!(click_key(col, m.y + 1, w, h, &all, 0), None);
        assert_eq!(click_key(col, m.y + 2, w, h, &all, 0), None);
        assert_eq!(click_key(m.x, m.y + 6, w, h, &all, 0), None);
        assert_eq!(click_key(0, 0, w, h, &all, 0).as_deref(), Some("esc"));
        assert_eq!(click_key(col, m.y + m.h, w, h, &all, 0).as_deref(), Some("esc"));
    }

    #[test]
    fn the_window_follows_the_highlight_only_as_far_as_it_must() {
        let all = choices();
        // In view: the window stays where the wheel left it.
        assert_eq!(follow_list(&all, 5, 10, 2), 2);
        // Below it: just far enough that it is on the last row.
        let at = drawn(&all).iter().position(|d| *d == Row::Item(20)).unwrap();
        assert_eq!(follow_list(&all, 20, 10, 0), at + 1 - 10);
        // Above it: it becomes the first row.
        assert_eq!(follow_list(&all, 0, 10, 8), 0);
        // The wheel stops with the last theme on the last row.
        assert_eq!(last_first(&all, 10), drawn(&all).len() - 10);
        let rows = inside_from(&all, 0, 999, "", "dusk", "", 60, 13);
        assert!(rows.iter().any(|r| r.contains(&all.last().unwrap().label)));
    }

    #[test]
    fn your_themes_sit_between_the_settings_and_the_bundled_ones() {
        let c = |v: &str, kind| Choice { value: v.into(), label: v.into(), detail: String::new(), kind };
        let list = vec![
            c("auto", Kind::Setting),
            c("terminal", Kind::Setting),
            c("mine", Kind::Yours),
            c("dusk", Kind::Bundled),
        ];
        let rows = drawn_with(&list, "~/.config/opscope/themes");
        assert_eq!(
            rows,
            vec![
                Row::Item(0),
                Row::Item(1),
                Row::Rule("your themes · ~/.config/opscope/themes".into()),
                Row::Item(2),
                Row::Rule("bundled".into()),
                Row::Item(3),
            ]
        );
        // Filtered to yours and the bundled: still headed apart.
        let rows = drawn_with(&list[2..], "~/t");
        assert_eq!(rows, vec![Row::Item(0), Row::Rule("bundled".into()), Row::Item(1)]);
        // In the real list, a user theme lands after terminal and before Dusk.
        let all = choices();
        assert_eq!(all[0].kind, Kind::Setting);
        assert!(all.windows(2).all(|w| w[0].kind <= w[1].kind), "sections out of order");
    }

    #[test]
    fn a_rule_parts_the_terminal_settings_from_the_themes() {
        let all = choices();
        // Tests read no themes folder: with none of yours, no section for them.
        let rows = inside(&all, 0, "", "dusk", "", 60, 12);
        assert!(rows[3].contains("terminal"), "{:?}", rows[3]);
        assert!(rows[4].contains("bundled") && rows[4].contains("──"), "{:?}", rows[4]);
        assert!(rows[5].contains("opscope Dusk"), "{:?}", rows[5]);
        assert!(!rows.iter().any(|r| r.contains("your themes")));
        // Only between the two kinds: none when a filter leaves one kind.
        let themes = filtered(&all, "nord");
        assert!(!inside(&themes, 0, "nord", "dusk", "", 60, 12).iter().any(|r| r.contains("──────")));
        // The rule takes a row, and the selection still stays in view.
        for sel in [0, 1, 2, 9, all.len() - 1] {
            let rows = inside(&all, sel, "", "dusk", "", 60, 8);
            assert!(rows.iter().any(|r| r.contains('▸')), "selection {sel} scrolled off");
        }
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
