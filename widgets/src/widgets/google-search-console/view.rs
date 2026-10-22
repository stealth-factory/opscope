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

//! The rows on screen, built from a reading and a width.
//!
//! Pure: no clock, no terminal, no network. The current time comes in on
//! `Screen`, so the same reading draws the same frame in a test as on a
//! wall, and every row can be measured against the width it was built for.

use std::collections::BTreeMap;

use chrono::{DateTime, Local, Utc};
use opscope_core as tc;

use crate::api::*;
use crate::parse::*;

/// Rows a section shows at once. The header says which slice it is
/// ("11-20 of 184") and the slice follows the cursor, so nothing is out of
/// reach and the sections below are not pushed a thousand rows down.
pub const SECTION_ROWS: usize = 10;

pub struct Palette {
    pub accent: String,
    pub lbl: String,
    pub txt: String,
    pub dim: String,
    pub up: String,
    pub down: String,
    pub warn: String,
    pub bar: String,
    /// Not text: the hatch and the dots under the hourly strip.
    pub grid: String,
}

/// Every colour drawn on a selected row clears AA 4.5 on the tint
/// `bg(28, 44, 62)`: accent 8.76, lbl 5.52, txt 11.77, dim 5.84, up 9.24,
/// down 5.95, warn 9.24.
pub fn palette() -> Palette {
    Palette {
        accent: tc::rgb(150, 210, 255),
        lbl: tc::rgb(130, 165, 200),
        txt: tc::rgb(225, 235, 245),
        dim: tc::rgb(150, 168, 190),
        up: tc::rgb(80, 235, 150),
        down: tc::rgb(255, 130, 135),
        warn: tc::rgb(255, 200, 90),
        bar: tc::rgb(120, 200, 255),
        grid: tc::rgb(96, 114, 138),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Pages,
    Queries,
    Terms,
    Index,
    Sitemaps,
    Sites,
}

pub const SECTIONS: [Section; 6] =
    [Section::Pages, Section::Queries, Section::Terms, Section::Index, Section::Sitemaps, Section::Sites];

impl Section {
    pub fn slot(self) -> usize {
        SECTIONS.iter().position(|s| *s == self).unwrap_or(0)
    }
}

/// Something a row can be: what `↵` opens and `c` copies.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Page(String),
    Query(String),
    Term(usize),
    Inspected(String),
    Sitemap(usize),
    Site(usize),
}

/// Everything one frame is drawn from.
pub struct Screen<'a> {
    pub sites: &'a [SiteData],
    pub at: usize,
    pub caches: &'a BTreeMap<String, InspectionCache>,
    pub metric: Metric,
    pub params: &'a Params,
    pub inspect_top: usize,
    pub needle: &'a str,
    pub typing: bool,
    /// What stops everything: no credential, sites.list refused, the
    /// poller died.
    pub err: &'a str,
    /// A key file others can read, or a pasted token's one-hour life.
    pub warning: &'a str,
    pub selected: usize,
    pub now: f64,
    pub busy: Option<usize>,
}

impl Screen<'_> {
    pub fn site(&self) -> Option<&SiteData> {
        self.sites.get(self.at)
    }
    pub fn cache(&self) -> Option<&InspectionCache> {
        self.site().and_then(|s| self.caches.get(&s.site))
    }
}

/// A built frame body: its rows, the selectable items in order, where each
/// was drawn, and the row the cursor is on.
pub struct Built {
    pub rows: Vec<String>,
    pub items: Vec<(Item, Section)>,
    pub placed: Vec<(usize, usize)>,
    pub cursor: Option<usize>,
}

// ------------------------------------------------------------- numbers --

/// `41,200`.
pub fn thousands(n: f64) -> String {
    let n = n.round() as i64;
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 { format!("−{}", out) } else { out }
}

/// `38.2k`, `1.2M`: for a figure beside a sentence, not in a column.
pub fn compact(n: f64) -> String {
    if n >= 1_000_000.0 {
        format!("{:.1}M", n / 1_000_000.0)
    } else if n >= 10_000.0 {
        format!("{:.1}k", n / 1000.0)
    } else {
        thousands(n)
    }
}

/// `+38%`, `−30%`.
pub fn signed_pct(p: f64) -> String {
    let r = p.round();
    if r > 0.0 {
        format!("+{}%", r as i64)
    } else if r < 0.0 {
        format!("−{}%", (-r) as i64)
    } else {
        "0%".into()
    }
}

/// A metric's value as it reads in a column.
pub fn value(metric: Metric, m: &Metrics) -> String {
    match metric {
        Metric::Impressions => thousands(m.impressions),
        Metric::Clicks => thousands(m.clicks),
        Metric::Ctr => format!("{:.1}%", m.ctr * 100.0),
        Metric::Position => format!("{:.1}", m.position),
    }
}

/// How a mover moved on `metric`, worded, and whether that is good.
/// `None` for the good flag means neither.
pub fn moved(mv: &Mover, metric: Metric) -> (String, Option<bool>) {
    if mv.is_new() {
        return ("new".into(), Some(true));
    }
    if mv.is_gone() {
        return ("gone".into(), Some(false));
    }
    let (Some(c), Some(p)) = (mv.cur, mv.prior) else { return ("–".into(), None) };
    match metric {
        Metric::Impressions | Metric::Clicks => {
            let (a, b) = (metric.of(&c), metric.of(&p));
            match percent(a, b) {
                Some(x) => (signed_pct(x), (x.round() != 0.0).then_some(x > 0.0)),
                None if a > 0.0 => ("new".into(), Some(true)),
                None => ("–".into(), None),
            }
        }
        Metric::Ctr => {
            let pp = (c.ctr - p.ctr) * 100.0;
            if pp.abs() < 0.05 {
                ("0pp".into(), None)
            } else {
                (format!("{}{:.1}pp", if pp > 0.0 { "+" } else { "−" }, pp.abs()), Some(pp > 0.0))
            }
        }
        Metric::Position => position_change(p.position - c.position),
    }
}

/// `▲0.6` when the average position improved (fell), `▼2.1` when it got
/// worse. Lower is better on Google, so the arrow follows the reader's
/// sense of better rather than the number's.
pub fn position_change(better_by: f64) -> (String, Option<bool>) {
    if better_by.abs() < 0.05 {
        ("0.0".into(), None)
    } else if better_by > 0.0 {
        (format!("▲{:.1}", better_by), Some(true))
    } else {
        (format!("▼{:.1}", -better_by), Some(false))
    }
}

fn verdict_colour<'a>(verdict: &str, p: &'a Palette) -> &'a str {
    match verdict {
        "PASS" => &p.up,
        "FAIL" | "ERROR" => &p.down,
        _ => &p.warn,
    }
}

fn good_colour(good: Option<bool>, p: &Palette) -> &str {
    match good {
        Some(true) => &p.up,
        Some(false) => &p.down,
        None => &p.dim,
    }
}

/// `Oct 18`.
fn day(d: chrono::NaiveDate) -> String {
    d.format("%b %-d").to_string()
}

/// An hour's start in this machine's local time.
pub fn local_hour(t: DateTime<Utc>) -> String {
    t.with_timezone(&Local).format("%H:%M").to_string()
}

// --------------------------------------------------------------- layout --

/// Break text into pieces no wider than `width` cells, at characters.
/// URLs and queries carry no spaces worth breaking at, and cutting one off
/// is the thing this collection does not do.
pub fn chunks(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = vec![String::new()];
    for ch in text.chars() {
        let cur = out.last_mut().expect("never empty");
        if tc::display_width(cur) + tc::display_width(&ch.to_string()) > width && !cur.is_empty() {
            out.push(String::new());
        }
        out.last_mut().expect("never empty").push(ch);
    }
    out
}

/// Text broken to `width`: at spaces when it has any, at characters when
/// it has none.
pub fn wrapped(text: &str, width: usize) -> Vec<String> {
    if text.trim().contains(' ') {
        tc::wrap_words(text, width.max(1))
    } else {
        chunks(text, width)
    }
}

/// Lay coloured pieces left to right, wrapping between them, with `lead`
/// before each line. A piece wider than a whole line is broken by
/// characters rather than clipped. Lines are built for `w - 1` cells, the
/// width every row here is drawn at.
pub fn flow(lead: &str, pieces: &[(&str, String)], sep: &str, w: usize) -> Vec<String> {
    flow_on(lead, pieces, sep, w, "")
}

/// `flow` on a background: `fill` (a tint, or nothing) runs under the lead
/// and out to the edge, so a selected row is tinted across its full width.
pub fn flow_on(lead: &str, pieces: &[(&str, String)], sep: &str, w: usize, fill: &str) -> Vec<String> {
    let room = w.saturating_sub(1).max(1);
    let lead_w = tc::display_width(lead).min(room.saturating_sub(1));
    let inner = room.saturating_sub(lead_w).max(1);
    let lead: String = lead.chars().take(lead_w).collect();
    let mut lines: Vec<Vec<(String, String)>> = vec![Vec::new()];
    let mut used = 0usize;
    let sep_w = tc::display_width(sep);
    for (colour, text) in pieces.iter().filter(|(_, t)| !t.is_empty()) {
        let tw = tc::display_width(text);
        if used > 0 && used + sep_w + tw > inner {
            lines.push(Vec::new());
            used = 0;
        } else if used > 0 {
            lines.last_mut().expect("never empty").push((colour.to_string(), sep.to_string()));
            used += sep_w;
        }
        if tw <= inner - used {
            lines.last_mut().expect("never empty").push((colour.to_string(), text.clone()));
            used += tw;
            continue;
        }
        // Wider than a whole line: prose breaks at its spaces, a URL or a
        // query with none breaks at characters. Never clipped.
        for (i, part) in wrapped(text, inner).into_iter().enumerate() {
            if i > 0 {
                lines.push(Vec::new());
            }
            used = tc::display_width(&part);
            lines.last_mut().expect("never empty").push((colour.to_string(), part));
        }
    }
    lines
        .into_iter()
        .filter(|l| !l.is_empty())
        .map(|mut l| {
            l.insert(0, (fill.to_string(), lead.clone()));
            if !fill.is_empty() {
                l.push((fill.to_string(), " ".repeat(room)));
            }
            let refs: Vec<(&str, String)> = l.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
            tc::seg(&refs, room)
        })
        .collect()
}

/// One message on its own, wrapped: an error, a note, a waiting line.
pub fn said(colour: &str, text: &str, w: usize) -> Vec<String> {
    let room = w.saturating_sub(1).max(1);
    tc::wrap_words(text, room.saturating_sub(3).max(1))
        .into_iter()
        .enumerate()
        .map(|(i, line)| tc::seg(&[(colour, format!("{}{}", if i == 0 { " ! " } else { "   " }, line))], room))
        .collect()
}

fn note_rows(colour: &str, text: &str, w: usize) -> Vec<String> {
    let room = w.saturating_sub(1).max(1);
    tc::wrap_words(text, room.saturating_sub(3).max(1))
        .into_iter()
        .map(|line| tc::seg(&[(colour, format!("   {}", line))], room))
        .collect()
}

fn heading(name: &str, about: Vec<(&str, String)>, p: &Palette, w: usize) -> Vec<String> {
    let mut pieces = vec![(p.lbl.as_str(), format!("── {} ──", name))];
    pieces.extend(about);
    flow(" ", &pieces, " · ", w)
}

/// What a part holds that is not a value: its error, its note, or that it
/// is still on its way. Empty when it has a fresh value.
fn part_rows<T>(part: &Part<T>, now: f64, p: &Palette, w: usize) -> Vec<String> {
    match (&part.value, &part.error, &part.note) {
        (Some(_), Some(err), _) => said(
            &p.warn,
            &format!("showing the reading from {} ago, because the newest failed: {}", tc::age(now - part.at), err),
            w,
        ),
        (Some(_), None, _) => Vec::new(),
        (None, Some(err), _) => said(&p.down, err, w),
        (None, None, Some(note)) => note_rows(&p.dim, note, w),
        (None, None, None) => note_rows(&p.dim, "waiting for Google…", w),
    }
}

/// The slice of a section on screen: where it starts, so that `at` (the
/// selected entry, if it is in this section) is inside it.
fn slice(start: &mut usize, len: usize, at: Option<usize>) -> (usize, usize) {
    if let Some(at) = at {
        if at < *start {
            *start = at;
        } else if at >= *start + SECTION_ROWS {
            *start = at + 1 - SECTION_ROWS;
        }
    }
    *start = (*start).min(len.saturating_sub(SECTION_ROWS));
    (*start, (*start + SECTION_ROWS).min(len))
}

fn showing(from: usize, to: usize, len: usize) -> String {
    if len == 0 {
        String::new()
    } else if len <= SECTION_ROWS {
        format!("{} shown", len)
    } else {
        format!("{}-{} of {}", from + 1, to, len)
    }
}

/// A row in a table: a lead cell, a name that may wrap onto further
/// lines, and right-aligned cells after it. Selected rows carry the tint
/// across the whole width.
struct Cells<'a> {
    here: bool,
    lead: (&'a str, String),
    name: (&'a str, String),
    cells: Vec<(&'a str, String, usize)>,
}

/// The narrowest a name column is drawn beside its cells. Narrower than
/// this and the cells go on lines of their own under the name instead.
const MIN_NAME: usize = 8;

/// Each cell as drawn: right-aligned in its column, or wider when what it
/// holds is wider than the column, so a long number is never cut.
fn cell_texts(cells: &[(&str, String, usize)]) -> Vec<String> {
    cells.iter().map(|(_, text, n)| format!(" {:>width$}", text, width = n)).collect()
}

/// Cells too wide to sit beside a name, packed onto as few lines as hold
/// them whole and right-aligned, each line as (colour, text) pieces.
fn stacked<'a>(cells: &[(&'a str, String, usize)], avail: usize) -> Vec<Vec<(&'a str, String)>> {
    let mut lines: Vec<Vec<(&str, String)>> = Vec::new();
    let mut used = 0;
    for ((colour, _, _), text) in cells.iter().zip(cell_texts(cells)) {
        let n = tc::display_width(&text);
        if lines.is_empty() || used + n > avail {
            lines.push(Vec::new());
            used = 0;
        }
        // A cell opening a line needs no gap before it.
        let text = if used == 0 { text.trim_start().to_string() } else { text };
        // A cell wider than a whole line is broken across lines rather
        // than cut; no number here is, but the rule holds anyway.
        for (k, piece) in chunks(&text, avail.max(1)).into_iter().enumerate() {
            if k > 0 {
                lines.push(Vec::new());
                used = 0;
            }
            used += tc::display_width(&piece);
            lines.last_mut().expect("a line was pushed").push((*colour, piece));
        }
    }
    lines
}

fn table(row: Cells, w: usize) -> Vec<String> {
    let room = w.saturating_sub(1).max(1);
    let tint = if row.here { tc::bg(28, 44, 62) } else { String::new() };
    let c = |colour: &str| format!("{}{}", tint, colour);
    let lead_w = tc::display_width(&row.lead.1);
    let texts = cell_texts(&row.cells);
    let cells_w: usize = texts.iter().map(|t| tc::display_width(t)).sum();
    // Side by side only when the name keeps a usable column; otherwise the
    // name takes the width and the cells follow it, so nothing is cut.
    let beside = lead_w + MIN_NAME + cells_w <= room;
    let name_w = if beside { room - lead_w - cells_w } else { room.saturating_sub(lead_w).max(1) };
    let parts = wrapped(&row.name.1, name_w);
    let finish = |mut line: Vec<(String, String)>| {
        line.push((tint.clone(), " ".repeat(room)));
        let refs: Vec<(&str, String)> = line.iter().map(|(a, b)| (a.as_str(), b.clone())).collect();
        tc::seg(&refs, room)
    };
    let mut out = Vec::new();
    for (i, part) in parts.iter().enumerate() {
        let mut line: Vec<(String, String)> = vec![
            (c(row.lead.0), if i == 0 { row.lead.1.clone() } else { " ".repeat(lead_w) }),
            (c(row.name.0), tc::pad(part, name_w)),
        ];
        if i == 0 && beside {
            for ((colour, _, _), text) in row.cells.iter().zip(&texts) {
                line.push((c(colour), text.clone()));
            }
        }
        out.push(finish(line));
    }
    if !beside {
        let avail = room.saturating_sub(lead_w).max(1);
        for pieces in stacked(&row.cells, avail) {
            let n: usize = pieces.iter().map(|(_, t)| tc::display_width(t)).sum();
            let mut line: Vec<(String, String)> = vec![(tint.clone(), " ".repeat(room.saturating_sub(n)))];
            line.extend(pieces.into_iter().map(|(colour, t)| (c(colour), t)));
            out.push(finish(line));
        }
    }
    out
}

/// A table's header, laid out by the same rule as its rows: labels beside
/// the name column when the rows' cells fit there, on their own line under
/// it when they do not.
fn header_row(lead: &str, name: &str, cells: &[(&str, usize)], p: &Palette, w: usize) -> Vec<String> {
    let room = w.saturating_sub(1).max(1);
    let lead_w = tc::display_width(lead);
    let labelled: Vec<(&str, String, usize)> = cells.iter().map(|(label, n)| (p.lbl.as_str(), label.to_string(), *n)).collect();
    let cells_w: usize = cell_texts(&labelled).iter().map(|t| tc::display_width(t)).sum();
    if lead_w + MIN_NAME + cells_w <= room {
        let name_w = room - lead_w - cells_w;
        let mut parts: Vec<(&str, String)> = vec![(p.lbl.as_str(), format!("{}{}", lead, tc::pad(name, name_w)))];
        parts.extend(cell_texts(&labelled).into_iter().map(|t| (p.lbl.as_str(), t)));
        return vec![tc::seg(&parts, room)];
    }
    let mut out = vec![tc::seg(&[(p.lbl.as_str(), format!("{}{}", lead, name))], room)];
    for pieces in stacked(&labelled, room.saturating_sub(lead_w).max(1)) {
        let n: usize = pieces.iter().map(|(_, t)| tc::display_width(t)).sum();
        let mut line: Vec<(&str, String)> = vec![("", " ".repeat(room.saturating_sub(n)))];
        line.extend(pieces);
        out.push(tc::seg(&line, room));
    }
    out
}

// ---------------------------------------------------------------- frame --

fn wanted(needle: &str, texts: &[&str]) -> bool {
    needle.is_empty() || texts.iter().any(|t| t.to_lowercase().contains(&needle.to_lowercase()))
}

/// The pages a section's index rows cover: the inspection set, plus any
/// page somebody inspected by hand in the last day.
pub fn inspected_rows<'a>(site: &SiteData, cache: Option<&'a InspectionCache>, top: usize, now: f64) -> (Vec<(&'a String, &'a Inspected)>, Vec<String>, usize, usize) {
    let movers = site.pages.value.as_ref().map(|m| m.rows.as_slice()).unwrap_or(&[]);
    let (set, tops, dropped) = inspection_set(movers, top);
    let Some(cache) = cache else { return (Vec::new(), set, tops, dropped) };
    let rows = cache
        .pages
        .iter()
        .filter(|(url, e)| set.contains(url) || now - e.at < REINSPECT_AFTER)
        .collect();
    let waiting = set.iter().filter(|u| !cache.pages.contains_key(*u)).cloned().collect();
    (rows, waiting, tops, dropped)
}

/// The index cell a page row carries: what its last inspection said.
pub fn index_cell(cache: Option<&InspectionCache>, url: &str, now: f64) -> (String, Option<bool>) {
    let Some(e) = cache.and_then(|c| c.pages.get(url)) else {
        return ("not inspected".into(), None);
    };
    match &e.result {
        Err(_) => ("refused · see INDEX".into(), Some(false)),
        Ok(i) => {
            let crawled = i
                .last_crawl
                .as_deref()
                .and_then(parse_rfc3339)
                .map(|t| format!(" · crawled {}", tc::age(now - t)))
                .unwrap_or_default();
            if i.verdict == "PASS" {
                (format!("ok{}", crawled), Some(true))
            } else {
                let what = if !i.fetch.is_empty() && i.fetch != "SUCCESSFUL" { &i.fetch } else { &i.verdict };
                (format!("{}{}", what, crawled), Some(false))
            }
        }
    }
}

/// Build the list screen.
pub fn list(s: &Screen, starts: &mut [usize; 6], p: &Palette, w: usize) -> Built {
    let mut rows = vec![tc::title_busy("search console", w, &p.accent, s.busy)];
    let site = s.site();
    let cache = s.cache();

    // Entries per section, filtered, in the order they are drawn.
    let mut entries: Vec<Vec<Item>> = vec![Vec::new(); 6];
    let mut pages: Vec<Mover> = Vec::new();
    let mut queries: Vec<Mover> = Vec::new();
    if let Some(site) = site {
        if let Some(m) = &site.pages.value {
            pages = m.rows.iter().filter(|r| wanted(s.needle, &[&r.key])).cloned().collect();
            sort_movers(&mut pages, s.metric);
            entries[0] = pages.iter().map(|m| Item::Page(m.key.clone())).collect();
        }
        if let Some(m) = &site.queries.value {
            queries = m.rows.iter().filter(|r| wanted(s.needle, &[&r.key])).cloned().collect();
            sort_movers(&mut queries, s.metric);
            entries[1] = queries.iter().map(|m| Item::Query(m.key.clone())).collect();
        }
        if let Some(t) = &site.terms.value {
            entries[2] = t
                .iter()
                .enumerate()
                .filter(|(_, t)| wanted(s.needle, &[&t.query, &t.page]))
                .map(|(i, _)| Item::Term(i))
                .collect();
        }
        let (inspected, _, _, _) = inspected_rows(site, cache, s.inspect_top, s.now);
        let mut bad: Vec<&(&String, &Inspected)> = inspected
            .iter()
            .filter(|(url, e)| !matches!(&e.result, Ok(i) if i.verdict == "PASS") && wanted(s.needle, &[url]))
            .collect();
        bad.sort_by_key(|(_, e)| match &e.result {
            Ok(i) if i.verdict == "FAIL" => 0,
            Ok(_) => 1,
            Err(_) => 2,
        });
        entries[3] = bad.iter().map(|(url, _)| Item::Inspected((*url).clone())).collect();
        if let Some(maps) = &site.sitemaps.value {
            entries[4] = maps
                .iter()
                .enumerate()
                .filter(|(_, m)| wanted(s.needle, &[&m.path]))
                .map(|(i, _)| Item::Sitemap(i))
                .collect();
        }
    }
    if s.sites.len() > 1 {
        entries[5] = (0..s.sites.len()).map(Item::Site).collect();
    }
    let items: Vec<(Item, Section)> = SECTIONS
        .iter()
        .enumerate()
        .flat_map(|(i, sec)| entries[i].iter().map(move |it| (it.clone(), *sec)))
        .collect();
    let selected = s.selected.min(items.len().saturating_sub(1));
    // Which section and which entry within it the cursor is on.
    let mut at_in: [Option<usize>; 6] = [None; 6];
    {
        let mut n = 0;
        for (i, e) in entries.iter().enumerate() {
            if selected >= n && selected < n + e.len() {
                at_in[i] = Some(selected - n);
            }
            n += e.len();
        }
    }
    let base = |slot: usize| entries[..slot].iter().map(|e| e.len()).sum::<usize>();
    let mut placed: Vec<(usize, usize)> = Vec::new();
    let mut cursor = None;

    // Status: which site, how old, how far Google's data reaches.
    let mut status: Vec<(&str, String)> = Vec::new();
    match site {
        Some(site) => {
            status.push((p.accent.as_str(), site.site.clone()));
            if s.sites.len() > 1 {
                status.push((p.dim.as_str(), format!("site {} of {}", s.at + 1, s.sites.len())));
            }
            status.push((p.dim.as_str(), format!("fetched {} ago", if site.fetched > 0.0 { tc::age(s.now - site.fetched) } else { "--".into() })));
            if let Some(h) = &site.hours.value {
                status.push((
                    p.dim.as_str(),
                    match h.latest_complete {
                        Some(t) => format!("hourly to {} (Google: a few hours behind)", local_hour(t)),
                        None => "no complete hour yet".into(),
                    },
                ));
            }
            if let Some(d) = &site.days.value {
                status.push(match (site.params.fresh, d.final_end, d.fresh_end) {
                    (false, Some(f), _) => (p.dim.as_str(), format!("final to {}", day(f))),
                    (false, None, _) => (p.warn.as_str(), "no final day yet".into()),
                    (true, _, Some(f)) => (p.warn.as_str(), format!("fresh to {}, Google may still change this", day(f))),
                    (true, _, None) => (p.warn.as_str(), "no day with data yet".into()),
                });
            }
            if site.params.kind != "web" {
                status.push((p.warn.as_str(), format!("search type {}", site.params.kind)));
            }
            if let Some(c) = cache {
                status.push((p.dim.as_str(), format!("inspections {}/{} today", thousands(c.used as f64), thousands(DAILY_INSPECTIONS as f64))));
            }
        }
        None => status.push((p.dim.as_str(), "no property to show yet".into())),
    }
    rows.extend(flow(" ", &status, "   ", w));
    if !s.err.is_empty() {
        rows.extend(said(&p.down, s.err, w));
    }
    if !s.warning.is_empty() {
        rows.extend(said(&p.warn, s.warning, w));
    }
    if let Some(problem) = site.and_then(|s| s.problem.as_deref()) {
        rows.extend(said(&p.down, problem, w));
    }
    if site.is_some_and(|d| d.params != *s.params) {
        rows.extend(note_rows(&p.warn, "asking Google again for the new window, data state or search type…", w));
    }
    let mut filters = Vec::new();
    if !s.needle.is_empty() {
        filters.push(format!("/{}", s.needle));
    }
    // The index rows are not counted on either side: they are a view of
    // the inspection cache, not rows Google returned for the window.
    let held: usize = entries[0].len() + entries[1].len() + entries[2].len() + entries[4].len();
    let all: usize = site
        .map(|d| {
            d.pages.value.as_ref().map_or(0, |m| m.rows.len())
                + d.queries.value.as_ref().map_or(0, |m| m.rows.len())
                + d.terms.value.as_ref().map_or(0, |t| t.len())
                + d.sitemaps.value.as_ref().map_or(0, |m| m.len())
        })
        .unwrap_or(0);
    let typing_help = if s.typing { "enter to keep · esc to clear" } else { "" };
    if let Some(line) = tc::filter_row(held, all, &filters) {
        let caret = if s.typing { format!("{}▏", line) } else { line };
        rows.extend(flow(" ", &[(p.warn.as_str(), caret), (p.dim.as_str(), typing_help.to_string())], "  ", w));
    } else if s.typing {
        rows.extend(flow(" ", &[(p.warn.as_str(), "filter: ▏".to_string()), (p.dim.as_str(), typing_help.to_string())], "  ", w));
    }
    if let Some(said_) = tc::filtered_to_nothing(all, &filters).filter(|_| held == 0) {
        rows.extend(note_rows(&p.warn, &said_, w));
    }

    let Some(site) = site else {
        return Built { rows, items, placed, cursor };
    };
    if site.problem.is_some() {
        return Built { rows, items, placed, cursor };
    }

    // LAST 24 HOURS
    rows.push(String::new());
    let hourly_head = match site.hours.value.as_ref().and_then(|h| h.hours.iter().find(|h| h.state != HourState::Complete)) {
        Some(first) if first.state == HourState::Collecting => {
            format!("Google marks hours from {} as still collecting", local_hour(first.start))
        }
        Some(first) => format!("Google has reported nothing from {} yet", local_hour(first.start)),
        None => String::new(),
    };
    rows.extend(heading(
        "LAST 24 HOURS",
        vec![
            (p.dim.as_str(), "clicks by hour, hourly_all".into()),
            (p.dim.as_str(), "local time; Google reports Pacific".into()),
            (p.warn.as_str(), hourly_head),
        ],
        p,
        w,
    ));
    rows.extend(part_rows(&site.hours, s.now, p, w));
    if let Some(h) = &site.hours.value {
        rows.extend(hours_rows(h, p, w));
    }

    // MOVERS · PAGES and MOVERS · QUERIES
    let windows = site.windows;
    let window_words = |p: &Palette| -> Vec<(String, String)> {
        let mut v = Vec::new();
        if let Some(((a, b), (c, d))) = windows {
            v.push((p.dim.clone(), format!("{} complete days ({}) vs prior {} ({})", site.params.days, span_label(a, b), site.params.days, span_label(c, d))));
        }
        v.push(if site.params.fresh {
            (p.warn.clone(), "fresh data: Google may still change this".to_string())
        } else {
            (p.dim.clone(), "final data, 2-3 days behind".to_string())
        });
        v
    };
    for (slot, name, part, list_) in [
        (0usize, "MOVERS · PAGES", &site.pages, &pages),
        (1usize, "MOVERS · QUERIES", &site.queries, &queries),
    ] {
        rows.push(String::new());
        let (from, to) = slice(&mut starts[slot], list_.len(), at_in[slot]);
        let mut about: Vec<(String, String)> = vec![(p.dim.clone(), s.metric.name().to_string())];
        about.extend(window_words(p));
        if slot == 1 {
            about.push((p.dim.clone(), "Google omits rare and anonymised queries, so rows sum below the totals".into()));
        }
        if part.value.as_ref().is_some_and(|m| m.capped) {
            about.push((p.warn.clone(), format!("top {} of more: Google's row limit", thousands(MOVER_ROWS as f64))));
        }
        about.push((p.dim.clone(), showing(from, to, list_.len())));
        let refs: Vec<(&str, String)> = about.iter().map(|(a, b)| (a.as_str(), b.clone())).collect();
        rows.extend(heading(name, refs, p, w));
        rows.extend(part_rows(part, s.now, p, w));
        if part.value.is_some() && list_.is_empty() && s.needle.is_empty() {
            rows.extend(note_rows(&p.dim, "no rows in either window", w));
        }
        if list_.is_empty() {
            continue;
        }
        let cols = mover_columns(s.metric, slot == 0, w);
        let labels: Vec<(&str, usize)> = cols.iter().map(|c| (c.label, c.width)).collect();
        rows.extend(header_row("  ", if slot == 0 { "PAGE" } else { "QUERY" }, &labels, p, w));
        for (i, mv) in list_.iter().enumerate().take(to).skip(from) {
            let here = at_in[slot] == Some(i);
            let start = rows.len();
            let name = if slot == 0 { short_page(&site.site, &mv.key) } else { mv.key.clone() };
            let cells: Vec<(&str, String, usize)> = cols
                .iter()
                .map(|c| {
                    let (colour, text) = (c.cell)(mv, cache, s.now);
                    (paint(colour, p), text, c.width)
                })
                .collect();
            rows.extend(table(
                Cells { here, lead: (p.accent.as_str(), if here { "▸ ".into() } else { "  ".into() }), name: (p.txt.as_str(), name), cells },
                w,
            ));
            if here {
                cursor = Some(rows.len() - 1);
            }
            let g = base(slot) + i;
            placed.extend((start..rows.len()).map(|r| (r, g)));
        }
    }

    // TOP SEARCH TERMS
    rows.push(String::new());
    let terms = site.terms.value.clone().unwrap_or_default();
    let shown_terms: Vec<usize> = entries[2].iter().filter_map(|i| if let Item::Term(n) = i { Some(*n) } else { None }).collect();
    let (from, to) = slice(&mut starts[2], shown_terms.len(), at_in[2]);
    let mut about: Vec<(String, String)> = vec![(p.dim.clone(), format!("top {} query and page pairs by clicks", TERM_ROWS))];
    if let Some(((a, b), _)) = windows {
        about.push((p.dim.clone(), span_label(a, b)));
    }
    about.push((p.dim.clone(), "Δ against the prior window's top pairs".into()));
    about.push((p.dim.clone(), "Google omits rare and anonymised queries".into()));
    about.push((p.dim.clone(), showing(from, to, shown_terms.len())));
    let refs: Vec<(&str, String)> = about.iter().map(|(a, b)| (a.as_str(), b.clone())).collect();
    rows.extend(heading("TOP SEARCH TERMS", refs, p, w));
    rows.extend(part_rows(&site.terms, s.now, p, w));
    if site.terms.value.as_ref().is_some_and(|t| t.is_empty()) {
        rows.extend(note_rows(&p.dim, "no search terms in this window", w));
    }
    for (i, &n) in shown_terms.iter().enumerate().take(to).skip(from) {
        let t = &terms[n];
        let here = at_in[2] == Some(i);
        let start = rows.len();
        rows.extend(term_rows(t, &site.site, here, p, w));
        if here {
            cursor = Some(rows.len() - 1);
        }
        let g = base(2) + i;
        placed.extend((start..rows.len()).map(|r| (r, g)));
    }

    // INDEX HEALTH
    rows.push(String::new());
    let (inspected, waiting, tops, dropped) = inspected_rows(site, cache, s.inspect_top, s.now);
    let used = cache.map(|c| c.used).unwrap_or(0);
    let bad = &entries[3];
    let (from, to) = slice(&mut starts[3], bad.len(), at_in[3]);
    rows.extend(heading(
        "INDEX HEALTH",
        vec![
            (p.dim.as_str(), format!("{} top pages + {} dropped, inspected daily", tops, dropped)),
            (p.dim.as_str(), format!("{}/{} today", thousands(used as f64), thousands(DAILY_INSPECTIONS as f64))),
            (p.dim.as_str(), "not the Page Indexing report (no API)".into()),
            (p.dim.as_str(), showing(from, to, bad.len())),
        ],
        p,
        w,
    ));
    if s.inspect_top == 0 {
        rows.extend(note_rows(&p.warn, &tc::missing_config("inspection is off: inspect_top is 0"), w));
    }
    if cache.is_some_and(|c| c.left() == 0) {
        rows.extend(said(
            &p.warn,
            "budget spent: today's 2,000 inspections are used, so nothing is inspected until Google's day turns over at midnight Pacific; each row says how long ago it was inspected",
            w,
        ));
    }
    if let Some(err) = &site.inspect_error {
        rows.extend(said(&p.down, err, w));
    }
    for (i, item) in bad.iter().enumerate().take(to).skip(from) {
        let Item::Inspected(url) = item else { continue };
        let Some(e) = cache.and_then(|c| c.pages.get(url)) else { continue };
        let here = at_in[3] == Some(i);
        let start = rows.len();
        rows.extend(inspection_rows(&site.site, url, e, here, s.now, p, w));
        if here {
            cursor = Some(rows.len() - 1);
        }
        let g = base(3) + i;
        placed.extend((start..rows.len()).map(|r| (r, g)));
    }
    let passing: Vec<&Inspected> = inspected
        .iter()
        .filter(|(_, e)| matches!(&e.result, Ok(i) if i.verdict == "PASS"))
        .map(|(_, e)| *e)
        .collect();
    if !passing.is_empty() {
        let oldest = passing
            .iter()
            .filter_map(|e| e.result.as_ref().ok().and_then(|i| i.last_crawl.as_deref()).and_then(parse_rfc3339))
            .map(|t| s.now - t)
            .fold(0.0f64, f64::max);
        rows.extend(flow(
            "  ",
            &[
                (p.up.as_str(), "PASS".into()),
                (p.txt.as_str(), format!("{} pages", passing.len())),
                (p.dim.as_str(), if oldest > 0.0 { format!("crawled within {}", tc::age(oldest)) } else { String::new() }),
            ],
            "   ",
            w,
        ));
    }
    if !waiting.is_empty() && s.inspect_top > 0 {
        rows.extend(note_rows(&p.dim, &format!("{} pages not inspected yet", waiting.len()), w));
    }
    if inspected.is_empty() && waiting.is_empty() && s.inspect_top > 0 && site.pages.value.is_some() {
        rows.extend(note_rows(&p.dim, "no pages with impressions to inspect", w));
    }

    // SITEMAPS
    rows.push(String::new());
    let maps = site.sitemaps.value.clone().unwrap_or_default();
    let shown_maps: Vec<usize> = entries[4].iter().filter_map(|i| if let Item::Sitemap(n) = i { Some(*n) } else { None }).collect();
    let (from, to) = slice(&mut starts[4], shown_maps.len(), at_in[4]);
    rows.extend(heading("SITEMAPS", vec![(p.dim.as_str(), showing(from, to, shown_maps.len()))], p, w));
    rows.extend(part_rows(&site.sitemaps, s.now, p, w));
    if site.sitemaps.value.as_ref().is_some_and(|m| m.is_empty()) {
        rows.extend(note_rows(&p.warn, "no sitemap submitted for this property", w));
    }
    for (i, &n) in shown_maps.iter().enumerate().take(to).skip(from) {
        let here = at_in[4] == Some(i);
        let start = rows.len();
        rows.extend(sitemap_rows(&site.site, &maps[n], here, s.now, p, w));
        if here {
            cursor = Some(rows.len() - 1);
        }
        let g = base(4) + i;
        placed.extend((start..rows.len()).map(|r| (r, g)));
    }

    // SITES
    if s.sites.len() > 1 {
        rows.push(String::new());
        let (from, to) = slice(&mut starts[5], s.sites.len(), at_in[5]);
        rows.extend(heading(
            "SITES",
            vec![(p.dim.as_str(), "each property's own totals, current window vs prior".into()), (p.dim.as_str(), showing(from, to, s.sites.len()))],
            p,
            w,
        ));
        for (i, d) in s.sites.iter().enumerate().take(to).skip(from) {
            let here = at_in[5] == Some(i);
            let start = rows.len();
            rows.extend(site_rows(d, s.caches.get(&d.site), here, s.inspect_top, s.now, p, w));
            if here {
                cursor = Some(rows.len() - 1);
            }
            let g = base(5) + i;
            placed.extend((start..rows.len()).map(|r| (r, g)));
        }
    }

    Built { rows, items, placed, cursor }
}

/// The hatched strip, its axis and its comparison.
pub fn hours_rows(h: &HourView, p: &Palette, w: usize) -> Vec<String> {
    let room = w.saturating_sub(1).max(1);
    let mut rows = Vec::new();
    if !h.any_rows {
        rows.extend(note_rows(
            &p.warn,
            "no hourly rows yet: Google has no hourly data for this property, which is usual for a new one. This is not zero clicks.",
            w,
        ));
        return rows;
    }
    let peak = |f: &dyn Fn(&Hour) -> f64| {
        h.hours.iter().filter(|x| x.state == HourState::Complete).map(f).fold(0.0f64, f64::max)
    };
    let strip = |f: &dyn Fn(&Hour) -> f64, hi: f64| -> Vec<(String, String)> {
        h.hours
            .iter()
            .map(|x| match x.state {
                HourState::Complete if f(x) <= 0.0 => (p.grid.clone(), "·".to_string()),
                HourState::Complete => {
                    let level = ((f(x) / hi.max(1e-9)) * 7.99) as usize;
                    (p.bar.clone(), tc::SPARK[level.min(7)].to_string())
                }
                _ => (p.grid.clone(), "░".to_string()),
            })
            .collect()
    };
    let inline = room >= 8 + h.hours.len();
    for (label, f) in [("clicks", &(|x: &Hour| x.clicks()) as &dyn Fn(&Hour) -> f64), ("impr", &|x: &Hour| x.impressions())] {
        let cells = strip(f, peak(f));
        let mut parts: Vec<(String, String)> = Vec::new();
        if inline {
            parts.push((p.dim.clone(), format!(" {:<7}", label)));
        } else {
            rows.push(tc::seg(&[(p.dim.as_str(), format!(" {}", label))], room));
            parts.push((String::new(), " ".into()));
        }
        parts.extend(cells);
        let refs: Vec<(&str, String)> = parts.iter().map(|(a, b)| (a.as_str(), b.clone())).collect();
        rows.push(tc::seg(&refs, room));
    }
    // The axis: first and last hour of the strip, in local time.
    let (first, last) = (h.hours.first(), h.hours.last());
    if let (Some(a), Some(b)) = (first, last) {
        let indent = if inline { 8 } else { 1 };
        let (a, b) = (local_hour(a.start), local_hour(b.start));
        let gap = h.hours.len().saturating_sub(a.len() + b.len()).max(1);
        rows.push(tc::seg(&[(p.dim.as_str(), format!("{}{}{}{}", " ".repeat(indent), a, " ".repeat(gap), b))], room));
    }
    let key: Vec<(&str, String)> = vec![
        (p.grid.as_str(), "░".into()),
        (p.dim.as_str(), "still collecting or not reported".into()),
        (p.grid.as_str(), "·".into()),
        (p.dim.as_str(), "a complete hour with nothing".into()),
    ];
    rows.extend(flow(" ", &key, " ", w));
    if h.now.hours == 0 {
        rows.extend(note_rows(&p.warn, "every hour in the last 24 is still collecting, so there is nothing complete to compare yet", w));
        return rows;
    }
    let mut cmp: Vec<(&str, String)> = vec![(p.dim.as_str(), format!("{} complete hours:", h.now.hours)), (p.txt.as_str(), format!("{} clicks", thousands(h.now.clicks)))];
    match &h.week_ago {
        Some(prior) => {
            cmp.push((p.dim.as_str(), format!("same hours last week {}", thousands(prior.clicks))));
            if let Some(x) = percent(h.now.clicks, prior.clicks) {
                cmp.push((good_colour((x.round() != 0.0).then_some(x > 0.0), p), signed_pct(x)));
            }
            let mut impr = format!("impressions {} vs {}", compact(h.now.impressions), compact(prior.impressions));
            if let Some(x) = percent(h.now.impressions, prior.impressions) {
                impr.push_str(&format!(" {}", signed_pct(x)));
            }
            cmp.push((p.dim.as_str(), impr));
            if let (Some(a), Some(b)) = (h.now.position, prior.position) {
                cmp.push((p.dim.as_str(), format!("avg position {:.1} ({:.1})", a, b)));
            }
        }
        None => {
            cmp.push((p.dim.as_str(), format!("impressions {}", compact(h.now.impressions))));
            cmp.push((
                p.dim.as_str(),
                if h.clock_change {
                    "no week-ago comparison: a Pacific clock change leaves these hours with no like-for-like hours a week back".into()
                } else {
                    "no week-ago comparison: Google's hourly lookback does not reach that far back".into()
                },
            ));
        }
    }
    rows.extend(flow(" ", &cmp, "  ", w));
    rows
}

/// One column of the movers tables. A cell names its colour by palette
/// field ("txt", "up", …) and `paint` turns that into the escape.
pub struct Col {
    pub label: &'static str,
    pub width: usize,
    #[allow(clippy::type_complexity)]
    pub cell: Box<dyn Fn(&Mover, Option<&InspectionCache>, f64) -> (&'static str, String)>,
}

fn tone(good: Option<bool>) -> &'static str {
    match good {
        Some(true) => "up",
        Some(false) => "down",
        None => "dim",
    }
}

/// What a mover's missing side is drawn as: zero when Google's answer was
/// whole, `–` when its row limit cut it and the row may sit just below.
fn absent(m: &Mover) -> String {
    if m.unknown { "–".into() } else { "0".into() }
}

/// Columns for a width: the metric and its change always, then the prior
/// value, the other count, position and the index state as the pane
/// allows. Extra width is spent on columns, never on padding.
pub fn mover_columns(metric: Metric, pages: bool, w: usize) -> Vec<Col> {
    let label = match metric {
        Metric::Impressions => "IMPR",
        Metric::Clicks => "CLICKS",
        Metric::Ctr => "CTR",
        Metric::Position => "POS",
    };
    let mut cols: Vec<Col> = vec![
        Col {
            label,
            width: 8,
            cell: Box::new(move |m, _, _| ("txt", m.cur.map(|c| value(metric, &c)).unwrap_or_else(|| absent(m)))),
        },
        Col {
            label: "Δ",
            width: 7,
            cell: Box::new(move |m, _, _| {
                let (t, g) = moved(m, metric);
                (tone(g), t)
            }),
        },
    ];
    if w >= 60 {
        cols.insert(
            1,
            Col {
                label: "PRIOR",
                width: 8,
                cell: Box::new(move |m, _, _| ("dim", m.prior.map(|c| value(metric, &c)).unwrap_or_else(|| absent(m)))),
            },
        );
    }
    if w >= 90 {
        let other = if metric == Metric::Clicks { Metric::Impressions } else { Metric::Clicks };
        let other_label = if other == Metric::Clicks { "CLICKS" } else { "IMPR" };
        cols.push(Col {
            label: other_label,
            width: 8,
            cell: Box::new(move |m, _, _| ("txt", m.cur.map(|c| value(other, &c)).unwrap_or_else(|| absent(m)))),
        });
        cols.push(Col {
            label: "Δ",
            width: 6,
            cell: Box::new(move |m, _, _| {
                let (t, g) = moved(m, other);
                (tone(g), t)
            }),
        });
        if metric != Metric::Position {
            cols.push(Col {
                label: "POS",
                width: 5,
                cell: Box::new(|m, _, _| ("txt", m.cur.map(|c| format!("{:.1}", c.position)).unwrap_or_else(|| "–".into()))),
            });
            cols.push(Col {
                label: "Δ",
                width: 6,
                cell: Box::new(|m, _, _| match (m.cur, m.prior) {
                    (Some(c), Some(pr)) => {
                        let (t, g) = position_change(pr.position - c.position);
                        (tone(g), t)
                    }
                    _ => ("dim", "–".into()),
                }),
            });
        }
    }
    if pages && w >= 120 {
        cols.push(Col {
            label: "INDEX",
            width: 24,
            cell: Box::new(|m, cache, now| {
                let (t, g) = index_cell(cache, &m.key, now);
                (if g == Some(false) { "down" } else { "dim" }, t)
            }),
        });
    }
    cols
}

/// The palette field a column named, as the escape to draw it in.
pub fn paint<'a>(name: &str, p: &'a Palette) -> &'a str {
    match name {
        "txt" => &p.txt,
        "up" => &p.up,
        "down" => &p.down,
        "warn" => &p.warn,
        _ => &p.dim,
    }
}

fn term_rows(t: &Term, site: &str, here: bool, p: &Palette, w: usize) -> Vec<String> {
    let tint = if here { tc::bg(28, 44, 62) } else { String::new() };
    let c = |colour: &str| format!("{}{}", tint, colour);
    let delta = match t.prior_clicks {
        Some(before) => match percent(t.m.clicks, before) {
            Some(x) => (good_colour((x.round() != 0.0).then_some(x > 0.0), p).to_string(), signed_pct(x)),
            None => (p.dim.clone(), "–".to_string()),
        },
        None => (p.dim.clone(), "–".to_string()),
    };
    let mut cells: Vec<(&str, String, usize)> = vec![(&p.txt, thousands(t.m.clicks), 7), (delta.0.as_str(), delta.1.clone(), 6)];
    if w >= 60 {
        cells.insert(1, (&p.dim, thousands(t.m.impressions), 8));
        cells.insert(2, (&p.dim, format!("{:.1}%", t.m.ctr * 100.0), 6));
        cells.insert(3, (&p.dim, format!("{:.1}", t.m.position), 5));
    }
    let mut out = table(
        Cells { here, lead: (p.accent.as_str(), if here { "▸ ".into() } else { "  ".into() }), name: (p.txt.as_str(), t.query.clone()), cells },
        w,
    );
    let mut landing: Vec<(String, String)> = vec![(c(&p.lbl), format!("→ {}", short_page(site, &t.page)))];
    if !t.others.is_empty() {
        landing.push((c(&p.warn), format!("  +{} {}", t.others.len(), if t.others.len() == 1 { "page" } else { "pages" })));
    }
    let landing_refs: Vec<(&str, String)> = landing.iter().map(|(a, b)| (a.as_str(), b.clone())).collect();
    out.extend(flow_on("    ", &landing_refs, "", w, &tint));
    out
}

fn inspection_rows(site: &str, url: &str, e: &Inspected, here: bool, now: f64, p: &Palette, w: usize) -> Vec<String> {
    let tint = if here { tc::bg(28, 44, 62) } else { String::new() };
    let c = |colour: &str| format!("{}{}", tint, colour);
    let (verdict, colour) = match &e.result {
        Ok(i) => (i.verdict.clone(), verdict_colour(&i.verdict, p)),
        Err(_) => ("ERROR".to_string(), p.down.as_str()),
    };
    let mut out = table(
        Cells {
            here,
            lead: (colour, format!("{}{:<8}", if here { "▸ " } else { "  " }, verdict)),
            name: (p.txt.as_str(), short_page(site, url)),
            cells: Vec::new(),
        },
        w,
    );
    let mut detail: Vec<(String, String)> = Vec::new();
    match &e.result {
        Ok(i) => {
            if !i.coverage.is_empty() {
                detail.push((c(&p.txt), i.coverage.clone()));
            }
            if !i.fetch.is_empty() {
                detail.push((c(if i.fetch == "SUCCESSFUL" { &p.dim } else { &p.down }), i.fetch.clone()));
            }
            if !i.indexing.is_empty() && i.indexing != "INDEXING_ALLOWED" {
                detail.push((c(&p.down), i.indexing.clone()));
            }
            if !i.robots.is_empty() && i.robots != "ALLOWED" {
                detail.push((c(&p.down), format!("robots.txt {}", i.robots)));
            }
            if i.canonical_differs() {
                detail.push((c(&p.warn), format!("Google chose {}", short_page(site, &i.google_canonical))));
            }
            if let Some(t) = i.last_crawl.as_deref().and_then(parse_rfc3339) {
                detail.push((c(&p.dim), format!("crawled {} ago", tc::age(now - t))));
            }
        }
        Err(err) => detail.push((c(&p.down), err.clone())),
    }
    detail.push((c(&p.dim), format!("inspected {} ago", tc::age(now - e.at))));
    let refs: Vec<(&str, String)> = detail.iter().map(|(a, b)| (a.as_str(), b.clone())).collect();
    let lead = if w >= 60 { "            " } else { "    " };
    out.extend(flow_on(lead, &refs, " · ", w, &tint));
    out
}

fn sitemap_rows(site: &str, m: &Sitemap, here: bool, now: f64, p: &Palette, w: usize) -> Vec<String> {
    let tint = if here { tc::bg(28, 44, 62) } else { String::new() };
    let c = |colour: &str| format!("{}{}", tint, colour);
    let mut out = table(
        Cells {
            here,
            lead: (p.accent.as_str(), if here { "▸ ".into() } else { "  ".into() }),
            name: (p.txt.as_str(), format!("{}{}", short_page(site, &m.path), if m.index { " (index)" } else { "" })),
            cells: Vec::new(),
        },
        w,
    );
    let mut detail: Vec<(String, String)> = Vec::new();
    detail.push((c(&p.dim), match m.urls() {
        Some(n) => format!("{} urls", thousands(n as f64)),
        None => "no url count".into(),
    }));
    match m.last_downloaded.as_deref().and_then(parse_rfc3339) {
        Some(t) if now - t >= 7.0 * 86400.0 => detail.push((c(&p.warn), format!("not read for {}", tc::age(now - t)))),
        Some(t) => detail.push((c(&p.dim), format!("read {} ago", tc::age(now - t)))),
        None => detail.push((c(&p.warn), "never read by Google".into())),
    }
    detail.push((c(if m.errors > 0 { &p.down } else { &p.dim }), format!("{} errors", m.errors)));
    detail.push((c(if m.warnings > 0 { &p.warn } else { &p.dim }), format!("{} warnings", m.warnings)));
    detail.push((c(if m.pending { &p.warn } else { &p.dim }), if m.pending { "pending".into() } else { "not pending".into() }));
    let refs: Vec<(&str, String)> = detail.iter().map(|(a, b)| (a.as_str(), b.clone())).collect();
    out.extend(flow_on("    ", &refs, " · ", w, &tint));
    out
}

/// One property's own totals over the two windows, from its daily rows.
pub fn site_totals(d: &SiteData) -> Option<(Totals, Totals)> {
    let (cur, prior) = d.windows?;
    let days = d.days.value.as_ref()?;
    Some((window_totals(&days.daily, cur), window_totals(&days.daily, prior)))
}

fn site_rows(d: &SiteData, cache: Option<&InspectionCache>, here: bool, top: usize, now: f64, p: &Palette, w: usize) -> Vec<String> {
    let tint = if here { tc::bg(28, 44, 62) } else { String::new() };
    let c = |colour: &str| format!("{}{}", tint, colour);
    let mut out = table(
        Cells { here, lead: (p.accent.as_str(), if here { "▸ ".into() } else { "  ".into() }), name: (p.txt.as_str(), d.site.clone()), cells: Vec::new() },
        w,
    );
    let mut detail: Vec<(String, String)> = Vec::new();
    if let Some(problem) = &d.problem {
        detail.push((c(&p.down), problem.clone()));
    } else if let Some((cur, prior)) = site_totals(d) {
        for (name, a, b) in [("clicks", cur.clicks, prior.clicks), ("impr", cur.impressions, prior.impressions)] {
            let (t, g) = match percent(a, b) {
                Some(x) => (signed_pct(x), (x.round() != 0.0).then_some(x > 0.0)),
                None => ("–".to_string(), None),
            };
            detail.push((c(&p.txt), format!("{} {}", name, compact(a))));
            detail.push((c(good_colour(g, p)), t));
        }
        if let (Some(a), Some(b)) = (cur.position, prior.position) {
            let (t, g) = position_change(b - a);
            detail.push((c(&p.txt), format!("pos {:.1}", a)));
            detail.push((c(good_colour(g, p)), t));
        }
    } else {
        detail.push((c(&p.dim), "no window to compare yet".into()));
    }
    let (inspected, _, _, _) = inspected_rows(d, cache, top, now);
    let bad = inspected.iter().filter(|(_, e)| !matches!(&e.result, Ok(i) if i.verdict == "PASS")).count();
    detail.push((c(if bad > 0 { &p.down } else { &p.dim }), format!("{} not PASS", bad)));
    if let Some(maps) = &d.sitemaps.value {
        let errors: i64 = maps.iter().map(|m| m.errors).sum();
        detail.push((c(if errors > 0 { &p.down } else { &p.dim }), format!("{} sitemap errors", errors)));
    }
    let refs: Vec<(&str, String)> = detail.iter().map(|(a, b)| (a.as_str(), b.clone())).collect();
    out.extend(flow_on("    ", &refs, "  ", w, &tint));
    out
}

// --------------------------------------------------------------- detail --

fn spark_rows(label: &str, values: &[f64], summary: String, p: &Palette, w: usize) -> Vec<String> {
    let room = w.saturating_sub(1).max(1);
    let lo = values.iter().cloned().fold(f64::INFINITY, f64::min);
    let hi = values.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let fits = room.saturating_sub(10).max(1);
    let shown = &values[values.len().saturating_sub(fits)..];
    let bars: String = shown
        .iter()
        .map(|v| {
            let f = if hi > lo { (v - lo) / (hi - lo) } else { 0.5 };
            tc::SPARK[((f * 7.99) as usize).min(7)]
        })
        .collect();
    let mut rows = vec![tc::seg(&[(p.dim.as_str(), format!(" {:<9}", label)), (p.bar.as_str(), bars)], room)];
    rows.extend(flow("           ", &[(p.dim.as_str(), summary)], "", w));
    rows
}

/// The rows of a detail screen, title first.
pub fn detail(s: &Screen, item: &Item, fetched: Option<&Result<Detail, String>>, p: &Palette, w: usize) -> Vec<String> {
    let mut rows = vec![tc::title_busy("search console", w, &p.accent, s.busy)];
    let Some(site) = s.site() else { return rows };
    let cache = s.cache();
    let field = |rows: &mut Vec<String>, name: &str, text: String, colour: &str| {
        rows.extend(flow(" ", &[(p.lbl.as_str(), format!("{:<11}", name)), (colour, text)], " ", w));
    };
    let series_rows = |rows: &mut Vec<String>, d: &Detail| {
        if d.series.is_empty() {
            rows.extend(note_rows(&p.dim, "no days with data in the last 28", w));
            return;
        }
        let n = d.series.len();
        rows.push(String::new());
        rows.extend(heading(
            &format!("LAST {} DAYS", DETAIL_DAYS),
            vec![(p.dim.as_str(), format!("{} days with data, to {}", n, day(d.series[n - 1].0)))],
            p,
            w,
        ));
        let clicks: Vec<f64> = d.series.iter().map(|(_, m)| m.clicks).collect();
        let impr: Vec<f64> = d.series.iter().map(|(_, m)| m.impressions).collect();
        let ctr: Vec<f64> = d.series.iter().map(|(_, m)| m.ctr * 100.0).collect();
        let pos: Vec<f64> = d.series.iter().map(|(_, m)| m.position).collect();
        let range = |v: &[f64], f: &dyn Fn(f64) -> String| {
            let lo = v.iter().cloned().fold(f64::INFINITY, f64::min);
            let hi = v.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
            format!("{} to {}", f(lo), f(hi))
        };
        rows.extend(spark_rows("clicks", &clicks, format!("total {}", thousands(clicks.iter().sum())), p, w));
        rows.extend(spark_rows("impr", &impr, format!("total {}", thousands(impr.iter().sum())), p, w));
        rows.extend(spark_rows("CTR", &ctr, range(&ctr, &|x| format!("{:.1}%", x)), p, w));
        rows.extend(spark_rows("position", &pos, format!("{} (lower is better)", range(&pos, &|x| format!("{:.1}", x))), p, w));
    };
    let top_rows = |rows: &mut Vec<String>, d: &Detail, title: &str, pages: bool| {
        rows.push(String::new());
        rows.extend(heading(title, vec![(p.dim.as_str(), "current window, by clicks".into())], p, w));
        if d.top.is_empty() {
            rows.extend(note_rows(&p.dim, "none in this window", w));
        }
        for r in &d.top {
            let key = r.keys.first().cloned().unwrap_or_default();
            let name = if pages { short_page(&site.site, &key) } else { key };
            rows.extend(table(
                Cells {
                    here: false,
                    lead: (p.dim.as_str(), "  ".into()),
                    name: (p.txt.as_str(), name),
                    cells: vec![(p.txt.as_str(), thousands(r.m.clicks), 7), (p.dim.as_str(), thousands(r.m.impressions), 8), (p.dim.as_str(), format!("{:.1}", r.m.position), 5)],
                },
                w,
            ));
        }
    };
    let fetched_rows = |rows: &mut Vec<String>, f: Option<&Result<Detail, String>>| -> Option<Detail> {
        match f {
            None => {
                rows.extend(note_rows(&p.dim, "asking Google for 28 days of this…", w));
                None
            }
            Some(Err(e)) => {
                rows.extend(said(&p.down, e, w));
                None
            }
            Some(Ok(d)) => Some(d.clone()),
        }
    };
    match item {
        Item::Page(url) | Item::Inspected(url) => {
            rows.extend(heading("PAGE", Vec::new(), p, w));
            rows.extend(flow(" ", &[(p.txt.as_str(), url.clone())], "", w));
            if let Some(mv) = site.pages.value.as_ref().and_then(|m| m.rows.iter().find(|r| r.key == *url)) {
                mover_fields(&mut rows, mv, p, w);
            }
            if let Some(d) = fetched_rows(&mut rows, fetched) {
                series_rows(&mut rows, &d);
                top_rows(&mut rows, &d, "TOP QUERIES", false);
            }
            rows.push(String::new());
            rows.extend(heading("INSPECTION", vec![(p.dim.as_str(), "the last one this widget asked for".into())], p, w));
            match cache.and_then(|c| c.pages.get(url)) {
                None => rows.extend(note_rows(
                    &p.dim,
                    "not inspected: press i to inspect it now, which spends one of today's 2,000",
                    w,
                )),
                Some(e) => {
                    field(&mut rows, "inspected", format!("{} ago", tc::age(s.now - e.at)), &p.dim);
                    match &e.result {
                        Err(err) => rows.extend(said(&p.down, err, w)),
                        Ok(i) => inspection_fields(&mut rows, site, i, s.now, p, w),
                    }
                }
            }
        }
        Item::Query(q) => {
            rows.extend(heading("QUERY", Vec::new(), p, w));
            rows.extend(flow(" ", &[(p.txt.as_str(), q.clone())], "", w));
            if let Some(mv) = site.queries.value.as_ref().and_then(|m| m.rows.iter().find(|r| r.key == *q)) {
                mover_fields(&mut rows, mv, p, w);
            }
            if let Some(d) = fetched_rows(&mut rows, fetched) {
                series_rows(&mut rows, &d);
                top_rows(&mut rows, &d, "PAGES RANKING", true);
            }
        }
        Item::Term(n) => {
            let Some(t) = site.terms.value.as_ref().and_then(|t| t.get(*n)) else { return rows };
            rows.extend(heading("SEARCH TERM", Vec::new(), p, w));
            rows.extend(flow(" ", &[(p.txt.as_str(), t.query.clone())], "", w));
            rows.push(String::new());
            rows.extend(heading(
                "LANDING PAGES",
                vec![(p.dim.as_str(), format!("{} in the top {} pairs", 1 + t.others.len(), TERM_ROWS))],
                p,
                w,
            ));
            if !t.others.is_empty() {
                rows.extend(note_rows(&p.warn, "more than one page ranks for this query, which can mean they compete for it", w));
            }
            for (page, m) in std::iter::once((&t.page, &t.m)).chain(t.others.iter().map(|(a, b)| (a, b))) {
                rows.extend(table(
                    Cells {
                        here: false,
                        lead: (p.dim.as_str(), "  ".into()),
                        name: (p.txt.as_str(), short_page(&site.site, page)),
                        cells: vec![(p.txt.as_str(), thousands(m.clicks), 7), (p.dim.as_str(), thousands(m.impressions), 8), (p.dim.as_str(), format!("{:.1}", m.position), 5)],
                    },
                    w,
                ));
            }
            if let Some(d) = fetched_rows(&mut rows, fetched) {
                series_rows(&mut rows, &d);
                top_rows(&mut rows, &d, "PAGES RANKING", true);
            }
        }
        Item::Sitemap(n) => {
            let Some(m) = site.sitemaps.value.as_ref().and_then(|m| m.get(*n)) else { return rows };
            rows.extend(heading("SITEMAP", Vec::new(), p, w));
            rows.extend(flow(" ", &[(p.txt.as_str(), m.path.clone())], "", w));
            field(&mut rows, "kind", if m.index { "sitemap index".into() } else { "sitemap".into() }, &p.txt);
            let when = |t: &Option<String>| match t.as_deref().and_then(parse_rfc3339) {
                Some(at) => format!("{} ago", tc::age(s.now - at)),
                None => "never".into(),
            };
            field(&mut rows, "submitted", when(&m.last_submitted), &p.txt);
            field(&mut rows, "read", when(&m.last_downloaded), &p.txt);
            field(&mut rows, "pending", if m.pending { "yes".into() } else { "no".into() }, &p.txt);
            field(&mut rows, "errors", m.errors.to_string(), if m.errors > 0 { &p.down } else { &p.txt });
            field(&mut rows, "warnings", m.warnings.to_string(), if m.warnings > 0 { &p.warn } else { &p.txt });
            for ct in &m.contents {
                field(
                    &mut rows,
                    &ct.kind,
                    ct.submitted.map(|n| format!("{} submitted", thousands(n as f64))).unwrap_or_else(|| "no count".into()),
                    &p.txt,
                );
            }
            if m.index {
                rows.push(String::new());
                rows.extend(heading("CHILD SITEMAPS", Vec::new(), p, w));
                match fetched {
                    None => rows.extend(note_rows(&p.dim, "asking Google for this index's sitemaps…", w)),
                    Some(Err(e)) => rows.extend(said(&p.down, e, w)),
                    Some(Ok(d)) => {
                        if let Some(e) = &d.children_error {
                            rows.extend(said(&p.down, e, w));
                        } else if d.children.is_empty() {
                            rows.extend(note_rows(&p.dim, "Google lists no child sitemaps", w));
                        }
                        for child in &d.children {
                            rows.extend(sitemap_rows(&site.site, child, false, s.now, p, w));
                        }
                    }
                }
            }
        }
        Item::Site(n) => {
            let Some(d) = s.sites.get(*n) else { return rows };
            rows.extend(heading("PROPERTY", Vec::new(), p, w));
            rows.extend(flow(" ", &[(p.txt.as_str(), d.site.clone())], "", w));
            field(&mut rows, "permission", if d.permission.is_empty() { "not in sites.list".into() } else { d.permission.clone() }, &p.txt);
            let c = s.caches.get(&d.site);
            field(
                &mut rows,
                "inspections",
                format!(
                    "{} used today, {} left (counted by this widget; the day is Google's, Pacific)",
                    thousands(c.map_or(0, |c| c.used) as f64),
                    thousands(c.map_or(DAILY_INSPECTIONS, |c| c.left()) as f64)
                ),
                &p.txt,
            );
            match d.hours.value.as_ref().and_then(|h| h.lookback) {
                Some((a, b)) => field(
                    &mut rows,
                    "hourly",
                    format!(
                        "Google returned {} to {} local, {} hours",
                        a.with_timezone(&Local).format("%b %-d %H:%M"),
                        b.with_timezone(&Local).format("%b %-d %H:%M"),
                        (b - a).num_hours() + 1
                    ),
                    &p.txt,
                ),
                None => field(&mut rows, "hourly", "no hourly rows returned".into(), &p.warn),
            }
            if let Some(days) = &d.days.value {
                field(&mut rows, "final to", days.final_end.map(day).unwrap_or_else(|| "no final day yet".into()), &p.txt);
                field(&mut rows, "fresh to", days.fresh_end.map(day).unwrap_or_else(|| "no day with data".into()), &p.txt);
            }
            if let Some(problem) = &d.problem {
                rows.extend(said(&p.down, problem, w));
            }
        }
    }
    rows
}

fn mover_fields(rows: &mut Vec<String>, mv: &Mover, p: &Palette, w: usize) {
    for metric in METRICS {
        let (t, g) = moved(mv, metric);
        let missing = || if mv.unknown { format!("below the top {}", thousands(MOVER_ROWS as f64)) } else { "none".into() };
        let now = mv.cur.map(|c| value(metric, &c)).unwrap_or_else(missing);
        let before = mv.prior.map(|c| value(metric, &c)).unwrap_or_else(missing);
        rows.extend(flow(
            " ",
            &[
                (p.lbl.as_str(), format!("{:<11}", metric.name())),
                (p.txt.as_str(), now),
                (p.dim.as_str(), format!("prior {}", before)),
                (good_colour(g, p), t),
            ],
            "  ",
            w,
        ));
    }
}

fn inspection_fields(rows: &mut Vec<String>, site: &SiteData, i: &Inspection, now: f64, p: &Palette, w: usize) {
    let field = |rows: &mut Vec<String>, name: &str, text: String, colour: &str| {
        if !text.is_empty() {
            rows.extend(flow(" ", &[(p.lbl.as_str(), format!("{:<11}", name)), (colour, text)], " ", w));
        }
    };
    field(rows, "verdict", i.verdict.clone(), verdict_colour(&i.verdict, p));
    field(rows, "coverage", i.coverage.clone(), &p.txt);
    field(rows, "fetch", i.fetch.clone(), if i.fetch == "SUCCESSFUL" { &p.txt } else { &p.down });
    field(rows, "indexing", i.indexing.clone(), if i.indexing == "INDEXING_ALLOWED" { &p.txt } else { &p.down });
    field(rows, "robots.txt", i.robots.clone(), if i.robots == "ALLOWED" { &p.txt } else { &p.down });
    if let Some(t) = i.last_crawl.as_deref().and_then(parse_rfc3339) {
        field(rows, "crawled", format!("{} ago, as {}", tc::age(now - t), if i.crawled_as.is_empty() { "unknown" } else { &i.crawled_as }), &p.txt);
    }
    field(rows, "canonical", i.user_canonical.clone(), &p.txt);
    field(rows, "Google's", i.google_canonical.clone(), if i.canonical_differs() { &p.warn } else { &p.txt });
    for (n, s) in i.sitemaps.iter().enumerate() {
        field(rows, if n == 0 { "sitemaps" } else { "" }, short_page(&site.site, s), &p.txt);
    }
    for (n, r) in i.referring.iter().enumerate() {
        field(rows, if n == 0 { "linked from" } else { "" }, short_page(&site.site, r), &p.txt);
    }
    if !i.rich_verdict.is_empty() {
        field(rows, "rich", i.rich_verdict.clone(), verdict_colour(&i.rich_verdict, p));
    }
    for issue in &i.rich {
        field(rows, "", format!("{}: {} ({})", issue.kind, issue.message, issue.severity), &p.warn);
    }
    if !i.link.is_empty() {
        field(rows, "in console", i.link.clone(), &p.dim);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};

    fn plain(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\u{1b}' {
                if chars.peek() == Some(&'[') {
                    chars.next();
                    for c in chars.by_ref() {
                        if c.is_ascii_alphabetic() {
                            break;
                        }
                    }
                } else {
                    // OSC: up to ST or BEL.
                    while let Some(c) = chars.next() {
                        if c == '\u{7}' {
                            break;
                        }
                        if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                continue;
            }
            out.push(c);
        }
        out
    }

    fn metrics(clicks: f64, impressions: f64, position: f64) -> Metrics {
        Metrics { clicks, impressions, ctr: if impressions > 0.0 { clicks / impressions } else { 0.0 }, position }
    }

    const LONG: &str = "https://example.com/docs/a-very-long-path/that-goes-on/and-on/past-any-narrow-pane/getting-started";

    fn params() -> Params {
        Params { fresh: false, days: 7, kind: "web".into() }
    }

    fn site(name: &str) -> SiteData {
        let now = Utc.with_ymd_and_hms(2026, 10, 21, 18, 0, 0).unwrap();
        let mut rows = Vec::new();
        let mut t = now - Duration::days(10);
        while t <= now {
            rows.push(Row { keys: vec![t.to_rfc3339()], m: metrics(10.0, 100.0, 8.0) });
            t += Duration::hours(1);
        }
        let hours = hour_view(
            &Analytics { rows, first_incomplete_hour: Some("2026-10-21T09:00:00-07:00".into()), first_incomplete_date: None },
            now,
        );
        let mut d = SiteData::new(name, &params());
        d.permission = "siteRestrictedUser".into();
        d.fetched = 1.0;
        d.hours = Part::ok(hours, 1.0);
        let end = parse_date("2026-10-18").unwrap();
        d.windows = Some(windows(end, 7));
        d.days = Part::ok(
            Days {
                final_end: Some(end),
                fresh_end: parse_date("2026-10-20"),
                daily: (0..20).map(|i| (end - Duration::days(i), metrics(100.0 + i as f64, 3000.0, 8.0))).collect(),
            },
            1.0,
        );
        let page = |k: &str, c: f64, i: f64, p: f64| Row { keys: vec![k.to_string()], m: metrics(c, i, p) };
        let cur = vec![
            page("https://example.com/pricing", 3120.0, 41200.0, 4.1),
            page(LONG, 640.0, 12100.0, 9.8),
            page("https://example.com/blog/launch", 410.0, 8300.0, 6.2),
        ];
        let prior = vec![
            page("https://example.com/pricing", 2210.0, 29800.0, 4.7),
            page(LONG, 980.0, 17400.0, 7.7),
            page("https://example.com/docs/old-guide", 200.0, 4100.0, 10.6),
        ];
        d.pages = Part::ok(Movers { rows: join_movers(&cur, &prior), capped: true }, 1.0);
        let q = |k: &str, c: f64| Row { keys: vec![k.to_string()], m: metrics(c, c * 20.0, 5.0) };
        d.queries = Part::ok(
            Movers { rows: join_movers(&[q("example widget pricing", 300.0), q("example", 50.0)], &[q("example widget pricing", 200.0)]), capped: false },
            1.0,
        );
        let pair = |a: &str, b: &str, c: f64| Row { keys: vec![a.to_string(), b.to_string()], m: metrics(c, c * 10.0, 3.0) };
        d.terms = Part::ok(
            group_terms(
                &[pair("example widget", "https://example.com/pricing", 90.0), pair("example widget", LONG, 4.0), pair("a long search term somebody typed into google", LONG, 3.0)],
                &[pair("example widget", "https://example.com/pricing", 80.0)],
            ),
            1.0,
        );
        d.sitemaps = Part::ok(
            parse_sitemaps(
                r#"{"sitemap":[{"path":"https://example.com/sitemap.xml","isSitemapsIndex":true,"lastDownloaded":"2026-10-01T00:00:00Z","errors":"2","warnings":"3","isPending":true,"contents":[{"type":"web","submitted":"1214"}]}]}"#,
            )
            .unwrap(),
            1.0,
        );
        d
    }

    fn cache() -> InspectionCache {
        let mut c = InspectionCache::default();
        let now = Utc.with_ymd_and_hms(2026, 10, 21, 18, 0, 0).unwrap().timestamp() as f64;
        c.roll("2026-10-21", now);
        let raw = |verdict: &str, fetch: &str| {
            format!(
                r#"{{"inspectionResult":{{"inspectionResultLink":"https://search.google.com/search-console/inspect","indexStatusResult":{{"verdict":"{verdict}","coverageState":"Submitted and indexed","pageFetchState":"{fetch}","indexingState":"INDEXING_ALLOWED","robotsTxtState":"ALLOWED","lastCrawlTime":"2026-10-20T08:00:00Z","googleCanonical":"https://example.com/pricing","userCanonical":"https://example.com/de/pricing"}}}}}}"#
            )
        };
        let a = raw("PASS", "SUCCESSFUL");
        c.record("https://example.com/pricing", now - 3600.0, a.clone(), parse_inspection(&a));
        let b = raw("FAIL", "SOFT_404");
        c.record(LONG, now - 3600.0, b.clone(), parse_inspection(&b));
        c.record(
            "https://example.com/docs/old-guide",
            now - 7200.0,
            String::new(),
            Err("HTTP 400 · Google said: URL is not in property, and this sentence is long enough to wrap at thirty columns".into()),
        );
        c
    }

    fn screen<'a>(sites: &'a [SiteData], caches: &'a BTreeMap<String, InspectionCache>, p: &'a Params, err: &'a str, selected: usize) -> Screen<'a> {
        Screen {
            sites,
            at: 0,
            caches,
            metric: Metric::Impressions,
            params: p,
            inspect_top: 25,
            needle: "",
            typing: false,
            err,
            warning: "",
            selected,
            now: Utc.with_ymd_and_hms(2026, 10, 21, 18, 0, 0).unwrap().timestamp() as f64,
            busy: None,
        }
    }

    const WIDTHS: [usize; 5] = [30, 40, 60, 100, 140];

    fn assert_fits(rows: &[String], w: usize, what: &str) {
        for (i, r) in rows.iter().enumerate().skip(1) {
            let n = tc::display_width(r);
            assert!(n < w, "{what} at {w} columns: row {i} is {n} wide: {:?}", plain(r));
        }
    }

    #[test]
    fn every_list_row_fits_at_every_width() {
        let sites = vec![site("sc-domain:example.com"), site("https://example.org/")];
        let mut caches = BTreeMap::new();
        caches.insert("sc-domain:example.com".to_string(), cache());
        let p = params();
        for w in WIDTHS {
            for selected in 0..20 {
                let mut starts = [0; 6];
                let b = list(&screen(&sites, &caches, &p, "", selected), &mut starts, &palette(), w);
                assert_fits(&b.rows, w, "the list");
                assert!(!b.items.is_empty());
                assert!(b.placed.iter().all(|(row, _)| *row < b.rows.len()));
            }
        }
    }

    #[test]
    fn a_table_row_keeps_every_cell_at_every_width() {
        let p = palette();
        for w in 12..=140 {
            for here in [false, true] {
                let cells = vec![(p.txt.as_str(), "12,345".to_string(), 7), (p.dim.as_str(), "1,234,567".to_string(), 8), (p.dim.as_str(), "4.1".to_string(), 5)];
                let rows = table(Cells { here, lead: (p.dim.as_str(), "  ".into()), name: (p.txt.as_str(), "/docs/a-page".into()), cells }, w);
                assert_fits(&rows, w + 1, "a table row");
                let text: Vec<String> = rows.iter().map(|r| plain(r)).collect();
                for cell in ["12,345", "1,234,567", "4.1"] {
                    assert!(text.iter().any(|r| r.contains(cell)), "{cell} lost at {w}: {text:?}");
                }
                let mut name: String = text.concat();
                for cell in ["12,345", "1,234,567", "4.1"] {
                    name = name.replacen(cell, "", 1);
                }
                let name: String = name.split_whitespace().collect();
                assert!(name.contains("/docs/a-page"), "the name lost at {w}: {text:?}");
            }
            let head = header_row("  ", "PAGE", &[("IMPR", 8), ("Δ", 7)], &p, w);
            assert_fits(&head, w + 1, "a header");
            let text: String = head.iter().map(|r| plain(r)).collect::<Vec<_>>().join("\n");
            assert!(text.contains("PAGE") && text.contains("IMPR") && text.contains('Δ'), "header lost at {w}: {text}");
        }
    }

    #[test]
    fn a_long_page_is_wrapped_never_cut() {
        let sites = vec![site("sc-domain:example.com")];
        let caches = BTreeMap::new();
        let p = params();
        for w in WIDTHS {
            let mut starts = [0; 6];
            let b = list(&screen(&sites, &caches, &p, "", 0), &mut starts, &palette(), w);
            let joined: String = b.rows.iter().map(|r| plain(r).trim().to_string()).collect::<Vec<_>>().join("");
            let compact: String = joined.split_whitespace().collect();
            assert!(compact.contains(&short_page("sc-domain:example.com", LONG)), "at {w} the long path is not whole");
        }
    }

    #[test]
    fn every_detail_row_fits_at_every_width() {
        let sites = vec![site("sc-domain:example.com"), site("https://example.org/")];
        let mut caches = BTreeMap::new();
        caches.insert("sc-domain:example.com".to_string(), cache());
        let p = params();
        let fetched: Result<Detail, String> = Ok(Detail {
            series: (0..28).map(|i| (parse_date("2026-09-21").unwrap() + Duration::days(i), metrics(i as f64, 100.0, 5.0))).collect(),
            top: vec![Row { keys: vec![LONG.into()], m: metrics(5.0, 50.0, 3.0) }],
            children: parse_sitemaps(r#"{"sitemap":[{"path":"https://example.com/child.xml"}]}"#).unwrap(),
            children_error: None,
        });
        let items = [Item::Page(LONG.into()), Item::Inspected("https://example.com/docs/old-guide".into()), Item::Query("example".into()), Item::Term(0), Item::Sitemap(0), Item::Site(1)];
        for w in WIDTHS {
            for item in &items {
                for f in [None, Some(&fetched), Some(&Err("429 quota: queries per minute exhausted (per-minute bucket, refills within a minute)".to_string()))] {
                    let rows = detail(&screen(&sites, &caches, &p, "", 0), item, f, &palette(), w);
                    assert_fits(&rows, w, &format!("{item:?}"));
                }
            }
        }
    }

    /// Every failure the spec names, each producing its own row, and each
    /// row fitting.
    #[test]
    fn every_failure_state_has_a_named_row() {
        let p = params();
        let base = site("sc-domain:example.com");
        let caches = BTreeMap::new();
        let mut cases: Vec<(&str, SiteData, &str, &str)> = Vec::new();
        let no_credential = opscope_core::google::missing_credential("google_search_console", "");
        cases.push(("no credential", base.clone(), no_credential.as_str(), "no credential"));
        let mut not_enabled = base.clone();
        not_enabled.problem = Some("API not enabled: turn on searchconsole.googleapis.com in the key's Cloud project (APIs & Services)".into());
        cases.push(("api not enabled", not_enabled, "", "API not enabled"));
        let mut denied = base.clone();
        denied.problem = Some(format!("403 no access: {}", grant_for(Some("reader@example-project.iam.gserviceaccount.com"), "sc-domain:example.com")));
        cases.push(("not a user", denied, "", "as a Restricted user"));
        let mut unverified = base.clone();
        unverified.problem = Some("siteUnverifiedUser: Search Console gives an unverified user no data".into());
        cases.push(("unverified", unverified, "", "siteUnverifiedUser"));
        let mut quota = base.clone();
        quota.pages = Part::failed("429 quota: queries per minute exhausted (per-minute bucket, refills within a minute)");
        cases.push(("429", quota, "", "429 quota"));
        let mut stale = base.clone();
        stale.sitemaps = Part::<Vec<Sitemap>>::failed("HTTP 500").after(base.sitemaps.clone());
        cases.push(("stale", stale, "", "showing the reading from"));
        let mut empty_hours = base.clone();
        empty_hours.hours = Part::ok(hour_view(&Analytics::default(), Utc::now()), 1.0);
        cases.push(("no hourly rows", empty_hours, "", "no hourly rows yet"));
        let mut collecting = base.clone();
        let now = Utc.with_ymd_and_hms(2026, 10, 21, 18, 0, 0).unwrap();
        collecting.hours = Part::ok(
            hour_view(
                &Analytics {
                    rows: vec![Row { keys: vec!["2026-10-19T10:00:00-07:00".into()], m: metrics(1.0, 1.0, 1.0) }],
                    first_incomplete_hour: Some("2026-10-19T11:00:00-07:00".into()),
                    first_incomplete_date: None,
                },
                now,
            ),
            1.0,
        );
        cases.push(("all collecting", collecting, "", "still collecting, so there is nothing complete"));
        let mut newborn = base.clone();
        newborn.pages = Part::noted("no final days yet: Google finalises a day 2-3 days after it ends, and a new property has none");
        cases.push(("no final days", newborn, "", "no final days yet"));
        cases.push(("capped", base.clone(), "", "top 1,000 of more"));
        cases.push(("poller died", base.clone(), "the poller stopped: index out of bounds (api.rs:1)", "the poller stopped"));
        let mut not_listed = base.clone();
        not_listed.problem = Some("https://example.com is not among the properties this account can see".into());
        cases.push(("not listed", not_listed, "", "not among the properties"));
        for (name, d, err, expect) in cases {
            let sites = vec![d];
            for w in WIDTHS {
                let mut starts = [0; 6];
                let b = list(&screen(&sites, &caches, &p, err, 0), &mut starts, &palette(), w);
                assert_fits(&b.rows, w, name);
                let text: String = b.rows.iter().map(|r| plain(r)).collect::<Vec<_>>().join(" ");
                let text: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
                assert!(text.contains(expect), "{name} at {w}: {expect:?} not on screen:\n{text}");
            }
        }
        // The inspection failures: a 400 outside the property on its row,
        // and a spent budget.
        let sites = vec![base.clone()];
        let mut caches = BTreeMap::new();
        let mut c = cache();
        c.used = DAILY_INSPECTIONS;
        caches.insert("sc-domain:example.com".to_string(), c);
        for w in WIDTHS {
            let mut starts = [0; 6];
            let b = list(&screen(&sites, &caches, &p, "", 0), &mut starts, &palette(), w);
            assert_fits(&b.rows, w, "inspection");
            let text: String = b.rows.iter().map(|r| plain(r)).collect::<Vec<_>>().join(" ");
            let text: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(text.contains("budget spent"), "{text}");
            assert!(text.contains("URL is not in property"), "{text}");
        }
    }

    #[test]
    fn hatched_and_zero_hours_draw_differently() {
        let now = Utc.with_ymd_and_hms(2026, 10, 21, 18, 0, 0).unwrap();
        let mut rows = Vec::new();
        let mut t = now - Duration::days(10);
        while t <= now {
            if t != now - Duration::hours(5) {
                rows.push(Row { keys: vec![t.to_rfc3339()], m: metrics(10.0, 100.0, 8.0) });
            }
            t += Duration::hours(1);
        }
        let h = hour_view(&Analytics { rows, first_incomplete_hour: Some("2026-10-21T09:00:00-07:00".into()), first_incomplete_date: None }, now);
        let rows = hours_rows(&h, &palette(), 100);
        let clicks = plain(&rows[0]);
        let strip: String = clicks.chars().skip(8).collect();
        assert_eq!(strip.chars().filter(|c| *c == '░').count(), 3);
        assert_eq!(strip.chars().filter(|c| *c == '·').count(), 1);
        assert!(strip.ends_with("░░░"));
    }

    #[test]
    fn the_index_section_states_what_was_inspected_and_collapses_pass() {
        let sites = vec![site("sc-domain:example.com")];
        let mut caches = BTreeMap::new();
        caches.insert("sc-domain:example.com".to_string(), cache());
        let p = params();
        let mut starts = [0; 6];
        let b = list(&screen(&sites, &caches, &p, "", 0), &mut starts, &palette(), 140);
        let text: String = b.rows.iter().map(|r| plain(r)).collect::<Vec<_>>().join("\n");
        assert!(text.contains("3 top pages + 1 dropped, inspected daily"), "{text}");
        assert!(text.contains("3/2,000 today"));
        assert!(text.contains("not the Page Indexing report (no API)"));
        assert!(text.contains("PASS   1 pages"));
        assert!(text.contains("SOFT_404"));
        // PASS rows are not items; the FAIL and the refused one are.
        let inspected: Vec<_> = b.items.iter().filter(|(_, s)| *s == Section::Index).collect();
        assert_eq!(inspected.len(), 2);
    }

    #[test]
    fn deltas_read_the_right_way_round() {
        let mv = Mover { key: "/a".into(), cur: Some(metrics(10.0, 1000.0, 4.1)), prior: Some(metrics(8.0, 800.0, 4.7)), unknown: false };
        assert_eq!(moved(&mv, Metric::Impressions), ("+25%".to_string(), Some(true)));
        assert_eq!(moved(&mv, Metric::Position), ("▲0.6".to_string(), Some(true)));
        assert_eq!(thousands(41200.0), "41,200");
        assert_eq!(compact(38200.0), "38.2k");
        assert_eq!(signed_pct(-30.2), "−30%");
    }

    #[test]
    fn the_cursor_and_its_rows_agree() {
        let sites = vec![site("sc-domain:example.com"), site("https://example.org/")];
        let caches = BTreeMap::new();
        let p = params();
        let mut starts = [0; 6];
        let b = list(&screen(&sites, &caches, &p, "", 4), &mut starts, &palette(), 100);
        let cursor = b.cursor.expect("a selected row");
        assert!(b.placed.iter().any(|(row, item)| *row == cursor && *item == 4));
    }
}
