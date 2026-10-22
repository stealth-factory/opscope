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

//! The screen, built as lines of coloured parts before anything is clipped.
//!
//! Every row is laid out to fit the width it is built for - a name that
//! does not fit its column wraps onto the next line rather than losing its
//! end - and the tests measure each line's plain width at every width the
//! issue names, so `seg()`'s clip is a guard that never has to fire.
#![allow(dead_code)]

use opscope_core as tc;

use crate::model::{self, Baseline, Sort};
use crate::parse::{Quota, Report};
use crate::poll::{Prop, Shared};

/// One row of the screen: `(colour, text)` parts.
pub type Line = Vec<(String, String)>;

pub struct Palette {
    pub txt: String,
    pub dim: String,
    pub head: String,
    pub accent: String,
    pub good: String,
    pub bad: String,
    pub warn: String,
    pub hint: String,
}

/// Every colour clears AA 4.5 on the terminal background and on the
/// selected-row tint; `every_colour_clears_aa_on_the_tint` measures it.
pub fn palette() -> Palette {
    Palette {
        txt: tc::rgb(225, 235, 245),
        dim: tc::rgb(160, 176, 196),
        head: tc::rgb(130, 200, 255),
        accent: tc::rgb(150, 210, 255),
        good: tc::rgb(110, 225, 150),
        bad: tc::rgb(255, 140, 140),
        warn: tc::rgb(255, 205, 110),
        hint: tc::rgb(160, 176, 196),
    }
}

/// The selected-row tint.
pub const TINT: (u8, u8, u8) = (38, 56, 76);

/// The metrics `m` cycles through, by the names GA uses.
pub const METRICS: &[(&str, &str)] = &[("screenPageViews", "views"), ("totalUsers", "users"), ("sessions", "sessions")];

/// Something a row can be: what `↵` opens and what the cursor lands on.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Event(String),
    Organic(String),
    Page(String),
    Channel(String),
    Source(String),
    Property(usize),
}

/// The sections `tab` moves between, in screen order.
pub const SECTIONS: usize = 4;

pub fn section_of(item: &Item) -> usize {
    match item {
        Item::Event(_) => 0,
        Item::Organic(_) => 1,
        Item::Page(_) | Item::Channel(_) | Item::Source(_) => 2,
        Item::Property(_) => 3,
    }
}

/// What the screen is showing and how.
pub struct View {
    pub prop: usize,
    pub metric: usize,
    pub sort: Sort,
    pub needle: String,
    pub typing: bool,
    pub selected: usize,
    pub configured: Vec<String>,
    pub weeks: usize,
    pub cred_note: Option<String>,
    pub now: f64,
}

/// A built body: its lines, the items on it, and where each landed.
pub struct Frame {
    pub lines: Vec<Line>,
    pub items: Vec<Item>,
    /// `(line, item)` for every line an item occupies.
    pub placed: Vec<(usize, usize)>,
    /// The last line of the selected item.
    pub cursor: Option<usize>,
}

/// The plain width of a line.
pub fn width_of(line: &Line) -> usize {
    line.iter().map(|(_, t)| tc::display_width(t)).sum()
}

pub fn plain(line: &Line) -> String {
    line.iter().map(|(_, t)| t.as_str()).collect()
}

fn part(colour: &str, text: impl Into<String>) -> (String, String) {
    (colour.to_string(), text.into())
}

/// Pad on the right to `n` cells; never cut.
fn left(s: &str, n: usize) -> String {
    let w = tc::display_width(s);
    format!("{}{}", s, " ".repeat(n.saturating_sub(w)))
}

/// Pad on the left to `n` cells; never cut.
fn right(s: &str, n: usize) -> String {
    let w = tc::display_width(s);
    format!("{}{}", " ".repeat(n.saturating_sub(w)), s)
}

/// Prose wrapped to the budget, each line led by `lead`.
fn prose(colour: &str, text: &str, budget: usize, lead: &str) -> Vec<Line> {
    let room = budget.saturating_sub(tc::display_width(lead)).max(1);
    tc::wrap_words(text, room)
        .into_iter()
        .enumerate()
        .map(|(i, l)| {
            let lead = if i == 0 { lead.to_string() } else { " ".repeat(tc::display_width(lead)) };
            vec![part(colour, format!("{}{}", lead, l))]
        })
        .collect()
}

/// Chunks laid left to right, wrapping between chunks, and a chunk wider
/// than the budget wrapped on its own as prose.
fn flow(chunks: Vec<Line>, budget: usize, sep: &str) -> Vec<Line> {
    let mut out: Vec<Line> = Vec::new();
    let mut cur: Line = vec![part("", " ")];
    let mut used = 1usize;
    let sep_w = tc::display_width(sep);
    for chunk in chunks {
        let w = width_of(&chunk);
        if w == 0 {
            continue;
        }
        if 1 + w > budget {
            if used > 1 {
                out.push(std::mem::replace(&mut cur, vec![part("", " ")]));
                used = 1;
            }
            let colour = chunk.first().map(|c| c.0.clone()).unwrap_or_default();
            out.extend(prose(&colour, &plain(&chunk), budget, " "));
            continue;
        }
        let need = if used > 1 { sep_w + w } else { w };
        if used + need > budget {
            out.push(std::mem::replace(&mut cur, vec![part("", " ")]));
            used = 1;
        } else if used > 1 {
            cur.push(part("", sep));
            used += sep_w;
        }
        used += w;
        cur.extend(chunk);
    }
    if used > 1 {
        out.push(cur);
    }
    out
}

/// A section heading and what it measures, wrapped.
fn heading(p: &Palette, name: &str, says: &str, budget: usize) -> Vec<Line> {
    let head = format!(" ── {} ──", name);
    let hw = tc::display_width(&head);
    if hw + 1 + tc::display_width(says) <= budget {
        return vec![vec![part(&p.head, head), part(&p.dim, format!(" {}", says))]];
    }
    let mut out = if hw <= budget { vec![vec![part(&p.head, head)]] } else { prose(&p.head, head.trim_start(), budget, " ") };
    out.extend(prose(&p.dim, says, budget, " "));
    out
}

/// One bar: `░` for an hour not processed, `·` for a real zero, and the
/// eighth blocks for the rest.
pub fn bar(values: &[Option<f64>], hi: f64) -> String {
    values
        .iter()
        .map(|v| match v {
            None => '░',
            Some(x) if *x <= 0.0 => '·',
            Some(x) => tc::SPARK[(((x / hi.max(1e-9)) * 7.99) as usize).min(7)],
        })
        .collect()
}

/// A table row: the name wrapped to `name_w` with the cells beside its
/// first line. `cells` are pre-padded.
#[allow(clippy::too_many_arguments)]
fn table_row(
    p: &Palette,
    mark: bool,
    here: bool,
    name: &str,
    name_colour: &str,
    name_w: usize,
    cells: &[(String, String)],
    budget: usize,
) -> Vec<Line> {
    let tint = if here { tc::bg(38, 56, 76) } else { String::new() };
    let c = |colour: &str| format!("{}{}", tint, colour);
    let cells_w: usize = cells.iter().map(|(_, t)| tc::display_width(t)).sum();
    // Too narrow for the name beside its figures: the name takes the
    // whole width and the figures go on the line under it.
    let stacked = stacks(name_w, cells_w, budget);
    let name_w = if stacked { budget.saturating_sub(1).max(1) } else { name_w };
    let pieces = tc::wrap_words(name, name_w.max(1));
    let mut out = Vec::new();
    let fill = |line: &mut Line| {
        if here {
            let used = width_of(line);
            line.push((tint.clone(), " ".repeat(budget.saturating_sub(used))));
        }
    };
    for (i, piece) in pieces.iter().enumerate() {
        let lead = if mark && here && i == 0 { "▸" } else { " " };
        let mut line: Line = vec![(c(&p.accent), lead.to_string()), (c(name_colour), left(piece, if stacked { 0 } else { name_w }))];
        if i == 0 && !stacked {
            for (colour, text) in cells {
                line.push((c(colour), text.clone()));
            }
        }
        fill(&mut line);
        out.push(line);
    }
    if stacked {
        let mut line: Line = vec![(c(&p.dim), " ".to_string())];
        for (colour, text) in cells {
            line.push((c(colour), text.clone()));
        }
        fill(&mut line);
        out.push(line);
    }
    out
}

/// Whether a table row puts its figures under its name.
fn stacks(name_w: usize, cells_w: usize, budget: usize) -> bool {
    name_w < 8 || 1 + name_w + cells_w > budget
}

/// A table's heading row, stacked the same way its rows are.
fn header(p: &Palette, first: &str, name_w: usize, cells: Vec<String>, budget: usize) -> Vec<Line> {
    let cells_w: usize = cells.iter().map(|t| tc::display_width(t)).sum();
    if stacks(name_w, cells_w, budget) || tc::display_width(first) > name_w {
        let mut second = vec![part(&p.head, " ")];
        second.extend(cells.into_iter().map(|t| part(&p.head, t)));
        return vec![vec![part(&p.head, format!(" {}", first))], second];
    }
    let mut line = vec![part(&p.head, format!(" {}", left(first, name_w)))];
    line.extend(cells.into_iter().map(|t| part(&p.head, t)));
    vec![line]
}

/// Quota as the title shows it: the category nearest its limit, by hour
/// and by day, as used of the documented limit. The hourly figure is the
/// tighter of the property's and this project's hourly buckets.
pub fn quota_words(rt: Option<&Quota>, core: Option<&Quota>, premium: bool) -> Option<(String, bool)> {
    let pick = [("realtime", rt), ("core", core)]
        .into_iter()
        .filter_map(|(n, q)| q.and_then(|q| q.tightest(premium).map(|t| (n, q, t.1))))
        .min_by(|a, b| a.2.total_cmp(&b.2))?;
    let (name, q, left) = pick;
    let words = |l: crate::parse::Level| format!("{}/{}", model::short(l.used), model::short(l.limit));
    let mut s = format!("quota {} used", name);
    let hour = ["tokensPerHour", "tokensPerProjectPerHour"]
        .iter()
        .filter_map(|k| q.level(k, premium))
        .min_by(|a, b| a.fraction_left().total_cmp(&b.fraction_left()));
    if let Some(h) = hour {
        s.push_str(&format!(" {} h", words(h)));
    }
    if let Some(d) = q.level("tokensPerDay", premium) {
        s.push_str(&format!(" {} d", words(d)));
    }
    Some((s, left < 0.10))
}

/// What the baseline row says, and the change against it when there is one.
pub fn baseline_words(b: &Baseline, now_value: f64, tz: chrono_tz::Tz) -> (String, Option<f64>) {
    let date = |t: f64| {
        use chrono::TimeZone;
        tz.timestamp_opt(t.floor() as i64, 0).single().map(|d| d.format("%b %-d").to_string()).unwrap_or_default()
    };
    match b {
        Baseline::Collecting { since } => (format!("baseline: collecting since {}", date(*since)), None),
        Baseline::Uncovered { label } => {
            (format!("no baseline for {} yet - the widget was not running then", label), None)
        }
        Baseline::Ready { median, found, of, since } => {
            let mut s = format!("baseline {} · from {} of {} weeks", model::thousands(*median), found, of);
            if let Some(t) = since {
                s.push_str(&format!(" · collecting since {}", date(*t)));
            }
            (s, model::pct(now_value, *median))
        }
    }
}

fn delta_colour<'a>(p: &'a Palette, d: Option<f64>) -> &'a str {
    match d {
        Some(x) if x >= 10.0 => &p.good,
        Some(x) if x <= -10.0 => &p.bad,
        _ => &p.dim,
    }
}

fn arrow(d: Option<f64>) -> &'static str {
    match d {
        Some(x) if x >= 10.0 => " ▲",
        Some(x) if x <= -10.0 => " ▼",
        _ => "",
    }
}

fn matches(name: &str, needle: &str) -> bool {
    needle.is_empty() || name.to_lowercase().contains(&needle.to_lowercase())
}

/// The status lines under the title: the property, how old each reading
/// is, the interval in force and why, and the quota.
pub fn status(shared: &Shared, v: &View, budget: usize, p: &Palette) -> Vec<Line> {
    let mut chunks: Vec<Line> = Vec::new();
    if let Some(prop) = shared.props.get(v.prop) {
        let name = if prop.name.is_empty() { "(name not read yet)".to_string() } else { prop.name.clone() };
        chunks.push(vec![part(&p.txt, format!("{} (properties/{})", name, prop.id))]);
        if shared.props.len() > 1 {
            chunks.push(vec![part(&p.dim, format!("←→ {} of {}", v.prop + 1, shared.props.len()))]);
        }
        let ago = |what: &str, t: f64| {
            if t > 0.0 {
                format!("{} {} ago", what, tc::ago(t))
            } else {
                format!("{}: none yet", what)
            }
        };
        chunks.push(vec![part(&p.dim, ago("realtime", prop.rt_at))]);
        chunks.push(vec![part(&p.dim, ago("breakdowns", prop.breakdown_at))]);
        chunks.push(vec![part(&p.dim, ago("reports", prop.core_at))]);
        let mut live = vec![part(&p.accent, format!("live {}s", prop.interval.max(1)))];
        if !prop.interval_why.is_empty() {
            live.push(part(&p.warn, format!(" ({})", prop.interval_why)));
        }
        chunks.push(live);
        if let Some((q, low)) = quota_words(prop.quota_rt.as_ref(), prop.quota_core.as_ref(), prop.premium) {
            chunks.push(vec![part(if low { &p.warn } else { &p.dim }, q)]);
        }
    }
    let mut out = flow(chunks, budget, "   ");
    // One failure is often the reason for several requests - a key that
    // will not sign blocks realtime, reports and the Admin API alike - and
    // saying it three times reads as three problems.
    let mut said: Vec<String> = Vec::new();
    let mut say = |colour: &str, text: &str| {
        if !text.is_empty() && !said.iter().any(|s| s == text) {
            said.push(text.to_string());
            out.extend(prose(colour, text, budget, " ! "));
        }
    };
    if let Some(note) = &v.cred_note {
        say(&p.warn, note);
    }
    say(&p.dim, &shared.discovery);
    say(&p.bad, &shared.discovery_err);
    if let Some(prop) = shared.props.get(v.prop) {
        say(&p.bad, &prop.died);
        if !prop.rt_banner.is_empty() {
            say(&p.bad, &format!("{} - realtime paused 5m, last reading kept below", prop.rt_banner));
        }
        if !prop.core_banner.is_empty() {
            say(&p.bad, &format!("{} - reports paused 5m, last reports kept below", prop.core_banner));
        }
        say(&p.bad, &prop.rt_err);
        say(&p.bad, &prop.bd_err);
        say(&p.bad, &prop.core_err);
        say(&p.bad, &prop.admin_err);
        say(&p.warn, &prop.sample_err);
    }
    out
}

/// The whole body under the title.
pub fn body(shared: &Shared, v: &View, w: usize) -> Frame {
    let p = palette();
    let budget = w.saturating_sub(1).max(1);
    let mut f = Frame { lines: Vec::new(), items: Vec::new(), placed: Vec::new(), cursor: None };
    f.lines.extend(status(shared, v, budget, &p));
    let Some(prop) = shared.props.get(v.prop) else {
        if shared.discovery.is_empty() && shared.discovery_err.is_empty() {
            f.lines.extend(prose(&p.dim, "no properties yet", budget, " "));
        }
        return f;
    };
    let tz = model::tz_of(&prop.time_zone);
    f.lines.push(Vec::new());
    right_now(&mut f, prop, v, budget, tz, &p);
    f.lines.push(Vec::new());
    today(&mut f, prop, v, budget, tz, &p);

    let mut counts = (0usize, 0usize);
    f.lines.push(Vec::new());
    key_events(&mut f, prop, v, budget, tz, &p, &mut counts);
    f.lines.push(Vec::new());
    organic(&mut f, prop, v, budget, &p, &mut counts);
    f.lines.push(Vec::new());
    movers(&mut f, prop, v, budget, &p, &mut counts);
    if shared.props.len() > 1 {
        f.lines.push(Vec::new());
        properties(&mut f, shared, v, budget, &p);
    }
    // The filter, named, with the count saying it is a subset - placed at
    // the top of the sections it narrows.
    if !v.needle.is_empty() || v.typing {
        let filters = if v.needle.is_empty() { Vec::new() } else { vec![format!("/{}", v.needle)] };
        let mut line = match tc::filter_row(counts.0, counts.1, &filters) {
            Some(said) => said,
            None => "filter: ".into(),
        };
        if v.typing {
            line.push_str("▏ enter keeps it, esc clears it");
        }
        let mut rows = prose(&p.warn, &line, budget, " ");
        if counts.0 == 0 {
            if let Some(none) = tc::filtered_to_nothing(counts.1, &filters) {
                rows.extend(prose(&p.dim, &none, budget, " "));
            }
        }
        let at = f.lines.len().min(status(shared, v, budget, &p).len());
        let n = rows.len();
        for (k, r) in rows.into_iter().enumerate() {
            f.lines.insert(at + k, r);
        }
        for pl in &mut f.placed {
            pl.0 += n;
        }
        if let Some(c) = f.cursor.as_mut() {
            *c += n;
        }
    }
    f
}

fn push_item(f: &mut Frame, v: &View, item: Item, lines: impl FnOnce(bool) -> Vec<Line>) {
    let index = f.items.len();
    let here = index == v.selected;
    let from = f.lines.len();
    f.lines.extend(lines(here));
    f.placed.extend((from..f.lines.len()).map(|row| (row, index)));
    if here {
        f.cursor = Some(f.lines.len().saturating_sub(1));
    }
    f.items.push(item);
}

fn event_base(prop: &Prop, v: &View, tz: chrono_tz::Tz, name: &str) -> Baseline {
    model::baseline(&prop.samples, v.now, v.weeks, tz, &|s| model::event_reading(s, name))
}

fn right_now(f: &mut Frame, prop: &Prop, v: &View, budget: usize, tz: chrono_tz::Tz, p: &Palette) {
    f.lines.extend(heading(p, "RIGHT NOW", "last 30 min · GA realtime · hits land within seconds", budget));
    let Some(rt) = &prop.rt else {
        let said = if prop.rt_err.is_empty() && prop.rt_banner.is_empty() {
            "waiting for the first realtime reading"
        } else {
            "no realtime reading - the reason is above"
        };
        f.lines.extend(prose(&p.dim, said, budget, " "));
        return;
    };
    let base = model::baseline(&prop.samples, v.now, v.weeks, tz, &|s| s.active);
    let (words, delta) = baseline_words(&base, rt.active, tz);
    let mut chunks = vec![vec![part(&p.dim, "active users "), part(&p.txt, model::thousands(rt.active))]];
    chunks.push(vec![part(&p.dim, words)]);
    if let Some(d) = delta {
        chunks.push(vec![part(delta_colour(p, Some(d)), format!("{}{}", model::pct_label(d), arrow(Some(d))))]);
    }
    f.lines.extend(flow(chunks, budget, "   "));
    if matches!(base, Baseline::Ready { .. }) {
        f.lines.extend(prose(
            &p.dim,
            "baseline = median active users at this minute of the week, from this widget's own samples",
            budget,
            " ",
        ));
    } else if let Some(line) = hourly_fallback(prop, v.now, tz) {
        f.lines.extend(prose(&p.dim, &line, budget, " "));
    }
    let hi = rt.minutes.iter().cloned().fold(0.0, f64::max);
    let series = bar(&rt.minutes.iter().map(|x| Some(*x)).collect::<Vec<_>>(), hi);
    if 13 + 30 <= budget {
        f.lines.push(vec![part(&p.dim, " per minute "), part(&p.good, series)]);
    } else {
        f.lines.push(vec![part(&p.dim, " per minute, oldest first")]);
        let chars: Vec<char> = series.chars().collect();
        for chunk in chars.chunks(budget.saturating_sub(1).max(1)) {
            f.lines.push(vec![part(&p.good, format!(" {}", chunk.iter().collect::<String>()))]);
        }
    }
    let last_min = rt.views_minutes.get(28).copied().unwrap_or(0.0);
    let mut chunks = vec![
        vec![part(&p.dim, "views last full minute "), part(&p.txt, model::thousands(last_min))],
        vec![part(&p.dim, "views 30m "), part(&p.txt, model::thousands(rt.views))],
    ];
    let events = prop
        .breakdown
        .as_ref()
        .map(|b| model::realtime_key_events(&b.events, &v.configured))
        .unwrap_or_default();
    if prop.breakdown.is_some() && events.is_empty() {
        chunks.push(vec![part(&p.dim, "key events 30m: none")]);
    }
    for (i, (name, n)) in events.iter().enumerate() {
        let base = match event_base(prop, v, tz, name) {
            Baseline::Ready { median, .. } => model::thousands(median),
            _ => "none yet".into(),
        };
        let lead = if i == 0 { "key events 30m: " } else { "" };
        chunks.push(vec![
            part(&p.dim, format!("{}{} ", lead, name)),
            part(&p.txt, model::thousands(*n)),
            part(&p.dim, format!(" (base {})", base)),
        ]);
    }
    f.lines.extend(flow(chunks, budget, "   "));
    match &prop.breakdown {
        None => f.lines.extend(prose(&p.dim, "pages, devices and countries arrive within a minute", budget, " ")),
        Some(b) => breakdowns(f, b, budget, p),
    }
    f.lines.extend(prose(
        &p.dim,
        "sources: not in GA realtime - see TODAY below (2–6 h behind)",
        budget,
        " ",
    ));
}

/// Today's last processed hour against the same hour last week - the
/// coarser comparison when no realtime baseline exists.
pub fn hourly_fallback(prop: &Prop, now: f64, tz: chrono_tz::Tz) -> Option<String> {
    let proc_ = prop.processed.as_ref()?;
    let h = model::hourly(&proc_.hourly, "sessions");
    let last = h.last?;
    let week = h.weeks.first()?.as_ref()?;
    let today = h.today[last].unwrap_or(0.0);
    let then = week[last];
    if proc_.date != model::local_now(now, tz).0 {
        return None;
    }
    let day = model::weekday(proc_.date);
    let change = model::pct(today, then).map(model::pct_label).unwrap_or_else(|| "no change to measure".into());
    Some(format!(
        "hourly instead: sessions {:02}:00-{:02}:59 today {} vs last {} {} ({}) · processed hourly data, not realtime",
        last,
        last,
        model::thousands(today),
        day,
        model::thousands(then),
        change
    ))
}

fn column(title: &str, rows: &[(String, f64)], width: usize, p: &Palette) -> Vec<Line> {
    let num_w = 5usize;
    let name_w = width.saturating_sub(num_w + 1).max(4);
    let mut out = vec![vec![part(&p.head, left(title, name_w)), part(&p.head, right("NOW", num_w + 1))]];
    if rows.is_empty() {
        out.push(vec![part(&p.dim, "nobody")]);
    }
    for (name, n) in rows {
        let name = if name.is_empty() { "(not set)" } else { name.as_str() };
        for (i, piece) in tc::wrap_words(name, name_w).iter().enumerate() {
            let mut line = vec![part(&p.txt, left(piece, name_w))];
            if i == 0 {
                line.push(part(&p.txt, right(&model::thousands(*n), num_w + 1)));
            }
            out.push(line);
        }
    }
    out
}

fn breakdowns(f: &mut Frame, b: &crate::poll::Breakdown, budget: usize, p: &Palette) {
    let lists = [
        ("PAGE (title)", model::top(&b.pages, "unifiedScreenName", "activeUsers", 5)),
        ("DEVICE", model::top(&b.devices, "deviceCategory", "activeUsers", 5)),
        ("COUNTRY", model::top(&b.countries, "country", "activeUsers", 5)),
    ];
    if budget >= 70 {
        let col = (budget - 1 - 4) / 3;
        let cols: Vec<Vec<Line>> = lists.iter().map(|(t, r)| column(t, r, col, p)).collect();
        let n = cols.iter().map(Vec::len).max().unwrap_or(0);
        for i in 0..n {
            let mut line: Line = vec![part("", " ")];
            for (k, c) in cols.iter().enumerate() {
                let cell = c.get(i).cloned().unwrap_or_default();
                let w = width_of(&cell);
                line.extend(cell);
                if k + 1 < cols.len() {
                    line.push(part("", " ".repeat(col.saturating_sub(w) + 2)));
                }
            }
            f.lines.push(line);
        }
    } else {
        for (t, r) in &lists {
            for mut line in column(t, r, budget.saturating_sub(1), p) {
                line.insert(0, part("", " "));
                f.lines.push(line);
            }
        }
    }
}

/// What a section fed by the processed reports says before it has any.
fn waiting_for_reports(prop: &Prop) -> &'static str {
    if prop.core_err.is_empty() && prop.core_banner.is_empty() {
        "waiting for the first processed reports"
    } else {
        "no processed reports - the reason is above"
    }
}

fn today(f: &mut Frame, prop: &Prop, v: &View, budget: usize, tz: chrono_tz::Tz, p: &Palette) {
    f.lines.extend(heading(
        p,
        "TODAY VS SAME WEEKDAY",
        "sessions by hour · GA says 2–6 h behind, not final for 24–48 h",
        budget,
    ));
    let Some(proc_) = &prop.processed else {
        f.lines.extend(prose(&p.dim, waiting_for_reports(prop), budget, " "));
        return;
    };
    let h = model::hourly(&proc_.hourly, "sessions");
    let day = model::weekday(proc_.date);
    let last_label = format!("last {}", day);
    if proc_.date != model::local_now(v.now, tz).0 {
        f.lines.extend(prose(
            &p.warn,
            &format!("these are {}'s reports, asked for before midnight - today's arrive with the next refresh", day),
            budget,
            " ",
        ));
    }
    let week: Vec<Option<f64>> = match h.weeks.first().and_then(|w| w.as_ref()) {
        Some(w) => w.iter().map(|x| Some(*x)).collect(),
        None => vec![None; 24],
    };
    let hi = h.today.iter().flatten().chain(week.iter().flatten()).cloned().fold(0.0, f64::max);
    let label_w = 1 + tc::display_width(&last_label).max(5) + 1;
    for (label, values, colour) in [("today", &h.today, &p.good), (last_label.as_str(), &week, &p.dim)] {
        let drawn = bar(values, hi);
        if label_w + 24 <= budget {
            f.lines.push(vec![part(&p.dim, left(&format!(" {}", label), label_w)), part(colour, drawn)]);
        } else {
            f.lines.push(vec![part(&p.dim, format!(" {}", label))]);
            f.lines.push(vec![part(colour, format!(" {}", drawn))]);
        }
    }
    f.lines.extend(prose(&p.dim, "░ = hour not yet processed, not zero · · = no sessions that hour", budget, " "));
    if proc_.hourly.rows.is_empty() && !proc_.hourly.meta.empty_reason.is_empty() {
        f.lines.extend(prose(&p.dim, &format!("GA: {}", proc_.hourly.meta.empty_reason), budget, " "));
    }
    let Some(vs) = model::versus(&h) else {
        f.lines.extend(prose(&p.warn, "today: no hour processed yet - GA runs 2–6 h behind", budget, " "));
        return;
    };
    let mut chunks = vec![vec![
        part(&p.dim, format!("to {:02}:00: today ", vs.upto + 1)),
        part(&p.txt, model::thousands(vs.today)),
    ]];
    match vs.last {
        Some(l) => {
            let d = model::pct(vs.today, l);
            chunks.push(vec![
                part(&p.dim, format!("{} ", last_label)),
                part(&p.txt, model::thousands(l)),
                part(delta_colour(p, d), d.map(|d| format!(" {}", model::pct_label(d))).unwrap_or_default()),
            ]);
        }
        None => chunks.push(vec![part(&p.dim, format!("{}: no data", last_label))]),
    }
    if let Some((m, n)) = vs.median {
        let d = model::pct(vs.today, m);
        chunks.push(vec![
            part(&p.dim, format!("{}-week median ", n)),
            part(&p.txt, model::thousands(m)),
            part(delta_colour(p, d), d.map(|d| format!(" {}", model::pct_label(d))).unwrap_or_default()),
        ]);
    }
    f.lines.extend(flow(chunks, budget, "  "));
    for (dim, word) in [("sessionSourceMedium", "top sources today"), ("sessionDefaultChannelGroup", "channels today")] {
        let split = model::split_to_hour(&proc_.sources, dim, "sessions", vs.upto);
        if split.is_empty() {
            continue;
        }
        let mut chunks = Vec::new();
        for (i, s) in split.iter().take(5).enumerate() {
            let d = model::pct(s.today, s.last);
            let change = if s.last <= 0.0 && s.today > 0.0 { "new".to_string() } else { d.map(model::pct_label).unwrap_or_default() };
            let mut chunk = Vec::new();
            if i == 0 {
                chunk.push(part(&p.dim, format!("{}: ", word)));
            }
            chunk.push(part(&p.txt, format!("{} {}", s.name, model::thousands(s.today))));
            if !change.is_empty() {
                chunk.push(part(delta_colour(p, d), format!(" ({})", change)));
            }
            chunks.push(chunk);
        }
        f.lines.extend(flow(chunks, budget, " · "));
    }
}

#[allow(clippy::too_many_arguments)]
fn key_events(f: &mut Frame, prop: &Prop, v: &View, budget: usize, tz: chrono_tz::Tz, p: &Palette, counts: &mut (usize, usize)) {
    let proc_ = prop.processed.as_ref();
    // The reports' own date, so the events are read for the same day the
    // sessions above are.
    let (date, day) = match proc_ {
        Some(x) => (x.date, model::weekday(x.date)),
        None => {
            let (d, _, day) = model::local_now(v.now, tz);
            (d, day)
        }
    };
    let h = proc_.map(|x| model::hourly(&x.hourly, "sessions"));
    let upto = h.as_ref().and_then(|h| h.last);
    let says = match upto {
        Some(u) => format!("today to {:02}:00 vs same weekday · same lag as above", u + 1),
        None => "today vs same weekday · same lag as above".into(),
    };
    f.lines.extend(heading(p, "KEY EVENTS", &says, budget));
    let names: Vec<String> = if v.configured.is_empty() {
        match &prop.key_names {
            Some(n) => n.clone(),
            None => {
                let said = if prop.admin_err.is_empty() {
                    "waiting for the property's key event list"
                } else {
                    "no key event list - the reason is above"
                };
                f.lines.extend(prose(&p.dim, said, budget, " "));
                return;
            }
        }
    } else {
        v.configured.clone()
    };
    if names.is_empty() {
        f.lines.extend(prose(
            &p.warn,
            "no key events configured - mark events as key events in GA (Admin, Data display, Key events)",
            budget,
            " ",
        ));
        return;
    }
    let Some(proc_) = proc_ else {
        f.lines.extend(prose(&p.dim, waiting_for_reports(prop), budget, " "));
        return;
    };
    let metric = if v.configured.is_empty() { "keyEvents" } else { "eventCount" };
    let lines = model::key_events(&proc_.key_events, &names, date, upto, metric);
    let vs = h.as_ref().and_then(model::versus);
    let wide = budget >= 60;
    let spark = budget >= 72;
    let last_head = format!("LAST {}", day.to_uppercase());
    let (tw, lw, dw, rw, sw) = (6usize, tc::display_width(&last_head).max(8) + 1, 13usize, 14usize, 9usize);
    let mut fixed = tw + lw + dw;
    if wide {
        fixed += rw;
    }
    if spark {
        fixed += sw;
    }
    let name_w = budget.saturating_sub(1 + fixed);
    let mut head = vec![right("TODAY", tw), right(&last_head, lw), left("  Δ", dw)];
    if wide {
        head.push(left("RATE/SESSION", rw));
    }
    if spark {
        head.push("LAST 7 D".into());
    }
    f.lines.extend(header(p, "EVENT", name_w, head, budget));
    for line in lines {
        counts.1 += 1;
        if !matches(&line.name, &v.needle) {
            continue;
        }
        counts.0 += 1;
        let processed = upto.is_some();
        let today_cell = if processed { model::thousands(line.today) } else { "░".into() };
        let d = model::pct(line.today, line.last);
        let (delta, dc) = if !processed {
            ("not processed".to_string(), p.dim.clone())
        } else if line.today == 0.0 && line.last > 0.0 {
            ("none today".to_string(), p.bad.clone())
        } else if line.last == 0.0 && line.today > 0.0 {
            ("new".to_string(), p.good.clone())
        } else {
            (d.map(|d| format!("{}{}", model::pct_label(d), arrow(Some(d)))).unwrap_or_else(|| "0".into()), delta_colour(p, d).to_string())
        };
        let mut cells = vec![
            (p.txt.clone(), right(&today_cell, tw)),
            (p.dim.clone(), right(&model::thousands(line.last), lw)),
            (dc, left(&format!("  {}", delta), dw)),
        ];
        if wide {
            let rate = |n: f64, s: Option<f64>| match s {
                Some(s) if s > 0.0 => format!("{:.1}%", n / s * 100.0),
                _ => "–".into(),
            };
            let words = match &vs {
                Some(vs) => format!("{} ({})", rate(line.today, Some(vs.today)), rate(line.last, vs.last)),
                None => "–".into(),
            };
            cells.push((p.dim.clone(), left(&words, rw)));
        }
        if spark {
            let hi = line.days.iter().cloned().fold(0.0, f64::max);
            let mut vals: Vec<Option<f64>> = line.days.iter().map(|x| Some(*x)).collect();
            vals[6] = None;
            cells.push((p.good.clone(), bar(&vals, hi)));
        }
        let name = line.name.clone();
        push_item(f, v, Item::Event(name.clone()), |here| table_row(p, true, here, &name, &p.txt, name_w, &cells, budget));
    }
    if upto.is_none() {
        f.lines.extend(prose(&p.dim, "today not yet processed - ░ is not zero", budget, " "));
    } else if spark {
        f.lines.extend(prose(&p.dim, "last 7 days: today ░ is still being processed", budget, " "));
    }
    let _ = date;
}

fn meta_note(r: &Report) -> Vec<&'static str> {
    let mut v = Vec::new();
    if r.meta.other_row {
        v.push("GA grouped some rows into (other)");
    }
    if r.meta.thresholded {
        v.push("GA withheld rows below its privacy thresholds");
    }
    if r.meta.sampled {
        v.push("GA sampled this data");
    }
    v
}

#[allow(clippy::too_many_arguments)]
fn mover_rows(
    f: &mut Frame,
    v: &View,
    p: &Palette,
    budget: usize,
    list: &[model::Mover],
    m: usize,
    end: chrono::NaiveDate,
    make: &dyn Fn(String) -> Item,
    spark: bool,
    limit: usize,
    counts: &mut (usize, usize),
) {
    let prior = budget >= 70;
    let (cw, pw, dw, sw) = (9usize, 9usize, 9usize, 16usize);
    let mut fixed = cw + dw;
    if prior {
        fixed += pw;
    }
    if spark {
        fixed += sw;
    }
    let name_w = budget.saturating_sub(1 + fixed);
    let mut shown = 0;
    for mv in list {
        counts.1 += 1;
        if !matches(&mv.name, &v.needle) {
            continue;
        }
        counts.0 += 1;
        if shown >= limit {
            continue;
        }
        shown += 1;
        let (c, pr) = (mv.cur.get(m).copied().unwrap_or(0.0), mv.prev.get(m).copied().unwrap_or(0.0));
        let d = model::pct(c, pr);
        let label = model::change_label(c, pr);
        let mut cells = vec![(p.txt.clone(), right(&model::thousands(c), cw))];
        if prior {
            cells.push((p.dim.clone(), right(&model::thousands(pr), pw)));
        }
        let colour = if label == "new" { p.good.clone() } else if label == "gone" { p.bad.clone() } else { delta_colour(p, d).to_string() };
        cells.push((colour, left(&format!("  {}{}", label, if label.ends_with('%') { arrow(d) } else { "" }), dw)));
        if spark {
            let days: Vec<(String, f64)> = mv.daily.iter().map(|(d, x)| (d.clone(), x.get(m).copied().unwrap_or(0.0))).collect();
            let tail: Vec<Option<f64>> = model::day_grid(&days, end, 14).into_iter().map(Some).collect();
            let hi = tail.iter().flatten().cloned().fold(0.0, f64::max);
            cells.push((p.good.clone(), format!("  {}", bar(&tail, hi))));
        }
        let name = if mv.name.is_empty() { "(not set)".to_string() } else { mv.name.clone() };
        let item_name = mv.name.clone();
        push_item(f, v, make(item_name), |here| table_row(p, true, here, &name, &p.txt, name_w, &cells, budget));
    }
}

fn mover_head(p: &Palette, budget: usize, first: &str, window: usize, spark: bool) -> Vec<Line> {
    let prior = budget >= 70;
    let (cw, pw, dw) = (9usize, 9usize, 9usize);
    let mut fixed = cw + dw;
    if prior {
        fixed += pw;
    }
    if spark {
        fixed += 16;
    }
    let name_w = budget.saturating_sub(1 + fixed);
    let mut head = vec![right(&format!("{}D", window), cw)];
    if prior {
        head.push(right("PRIOR", pw));
    }
    head.push(left("  Δ", dw));
    if spark {
        head.push("  LAST 14 DAYS".into());
    }
    header(p, first, name_w, head, budget)
}

fn organic(f: &mut Frame, prop: &Prop, v: &View, budget: usize, p: &Palette, counts: &mut (usize, usize)) {
    let window = prop.processed.as_ref().map(|x| x.window).unwrap_or(7);
    f.lines.extend(heading(
        p,
        "TOP ORGANIC LANDING PAGES",
        &format!("Google Search visitors · sessions · last {} days vs prior {}", window, window),
        budget,
    ));
    f.lines.extend(prose(
        &p.dim,
        "GA4 does not give the search terms - Google withholds them as \"not provided\" and the Data API has no Search Console query dimension. The google-search-console widget shows the terms and the page each lands on.",
        budget,
        " ",
    ));
    let Some(r) = prop.processed.as_ref().and_then(|x| x.organic.as_ref()) else {
        let said = if prop.processed.is_some() { "no organic report - the reason is above" } else { waiting_for_reports(prop) };
        f.lines.extend(prose(&p.dim, said, budget, " "));
        return;
    };
    if let Some(proc_) = &prop.processed {
        if let Some(line) = kept_since(proc_, prop) {
            f.lines.extend(prose(&p.warn, &line, budget, " "));
        }
    }
    if !v.configured.is_empty() {
        f.lines.extend(prose(
            &p.dim,
            "KEY EV here counts the events GA flags as key events, not the key_events list in config - GA cannot count chosen events per landing page without narrowing the sessions to them",
            budget,
            " ",
        ));
    }
    let mut list = model::movers(r, &|r, row| r.d(row, "landingPage").to_string(), &["sessions", "keyEvents"]);
    model::sort_movers(&mut list, 0, Sort::Size);
    if list.is_empty() {
        let why = if r.meta.empty_reason.is_empty() { "no Google Search sessions in either window".to_string() } else { format!("GA: {}", r.meta.empty_reason) };
        f.lines.extend(prose(&p.dim, &why, budget, " "));
        return;
    }
    let wide = budget >= 60;
    let (sw, kw, dw) = (9usize, 8usize, 9usize);
    let fixed = sw + dw + if wide { kw } else { 0 };
    let name_w = budget.saturating_sub(1 + fixed);
    let mut head = vec![right("SESSIONS", sw)];
    if wide {
        head.push(right("KEY EV", kw));
    }
    head.push(left("  Δ", dw));
    f.lines.extend(header(p, "LANDING PAGE", name_w, head, budget));
    let mut shown = 0;
    for mv in &list {
        counts.1 += 1;
        if !matches(&mv.name, &v.needle) {
            continue;
        }
        counts.0 += 1;
        if shown >= 8 {
            continue;
        }
        shown += 1;
        let d = model::pct(mv.cur[0], mv.prev[0]);
        let label = model::change_label(mv.cur[0], mv.prev[0]);
        let mut cells = vec![(p.txt.clone(), right(&model::thousands(mv.cur[0]), sw))];
        if wide {
            cells.push((p.dim.clone(), right(&model::thousands(mv.cur[1]), kw)));
        }
        cells.push((delta_colour(p, d).to_string(), left(&format!("  {}", label), dw)));
        let name = if mv.name.is_empty() { "(not set)".to_string() } else { mv.name.clone() };
        let item = mv.name.clone();
        push_item(f, v, Item::Organic(item), |here| table_row(p, true, here, &name, &p.txt, name_w, &cells, budget));
    }
    let mut notes: Vec<String> = meta_note(r).into_iter().map(str::to_string).collect();
    notes.extend(cut_note(r, "landing-page rows", "pages near the bottom may read new or gone only because GA left out their other window"));
    if !notes.is_empty() {
        f.lines.extend(prose(&p.warn, &notes.join(" · "), budget, " "));
    }
}

/// When a report stopped short of all its rows: GA's `rowCount` beyond
/// what came back. Said, because a sum over a cut report is not a total.
fn cut_note(r: &Report, rows: &str, so: &str) -> Option<String> {
    (r.row_count > r.rows.len()).then(|| format!("GA returned {} of {} {}, so {}", r.rows.len(), r.row_count, rows, so))
}

/// The window reports' age, when their last refresh failed and an older
/// answer is on screen.
fn kept_since(proc_: &crate::poll::Processed, prop: &Prop) -> Option<String> {
    (proc_.window_at > 0.0 && proc_.window_at + 1.0 < prop.core_at)
        .then(|| format!("window totals and organic pages read {} ago - the latest refresh of them failed, reason above", tc::ago(proc_.window_at)))
}

fn movers(f: &mut Frame, prop: &Prop, v: &View, budget: usize, p: &Palette, counts: &mut (usize, usize)) {
    let Some(proc_) = &prop.processed else {
        f.lines.extend(heading(p, "MOVERS", "pages, channels and sources · window vs the one before", budget));
        f.lines.extend(prose(&p.dim, waiting_for_reports(prop), budget, " "));
        return;
    };
    let (metric, word) = METRICS[v.metric % METRICS.len()];
    let m = v.metric % METRICS.len();
    const MET: &[&str] = &["screenPageViews", "totalUsers", "sessions"];
    let by_day = model::movers(&proc_.pages, &|r, row| r.d(row, "pagePath").to_string(), MET);
    let mut pages = match &proc_.page_totals {
        Some(t) => {
            let mut v = model::movers(t, &|r, row| r.d(row, "pagePath").to_string(), MET);
            model::attach_daily(&mut v, &by_day);
            v
        }
        None => Vec::new(),
    };
    model::sort_movers(&mut pages, m, v.sort);
    let _ = metric;
    let n = pages.len();
    let shown = n.min(10);
    f.lines.extend(heading(
        p,
        "MOVERS",
        &format!(
            "{} · last {} days vs prior {} · by {} · pages 1-{} of {}",
            word,
            proc_.window,
            proc_.window,
            v.sort.word(),
            shown,
            n
        ),
        budget,
    ));
    if let Some(line) = kept_since(proc_, prop) {
        f.lines.extend(prose(&p.warn, &line, budget, " "));
    }
    let Some(totals) = &proc_.page_totals else {
        f.lines.extend(prose(&p.dim, "no window totals yet - the reason is above", budget, " "));
        return;
    };
    let mut notes: Vec<String> = meta_note(totals).into_iter().map(str::to_string).collect();
    notes.extend(cut_note(totals, "page rows", "the smallest pages may be missing"));
    notes.extend(cut_note(&proc_.pages, "page-day rows", "the charts of the smallest pages may be missing days"));
    if !notes.is_empty() {
        f.lines.extend(prose(&p.warn, &notes.join(" · "), budget, " "));
    }
    let end = proc_.date.pred_opt().unwrap_or(proc_.date);
    if pages.is_empty() {
        let why = if totals.meta.empty_reason.is_empty() { "no page views in either window".to_string() } else { format!("GA: {}", totals.meta.empty_reason) };
        f.lines.extend(prose(&p.dim, &why, budget, " "));
    } else {
        let spark = budget >= 120;
        f.lines.extend(mover_head(p, budget, "PAGE", proc_.window, spark));
        mover_rows(f, v, p, budget, &pages, m, end, &Item::Page, spark, 10, counts);
    }
    let channel = proc_.channels.as_ref().map(|r| (r, "sessionDefaultChannelGroup", "CHANNEL", &Item::Channel as &dyn Fn(String) -> Item));
    let source = Some((&proc_.source_movers, "sessionSourceMedium", "SOURCE / MEDIUM", &Item::Source as &dyn Fn(String) -> Item));
    for (r, dim, title, make) in [channel, source].into_iter().flatten() {
        let mut list = model::movers(r, &|r, row| r.d(row, dim).to_string(), MET);
        model::sort_movers(&mut list, m, v.sort);
        if list.is_empty() {
            continue;
        }
        f.lines.push(Vec::new());
        f.lines.extend(mover_head(p, budget, title, proc_.window, false));
        mover_rows(f, v, p, budget, &list, m, end, make, false, 8, counts);
        let mut notes: Vec<String> = meta_note(r).into_iter().map(str::to_string).collect();
        notes.extend(cut_note(r, "rows", "the smallest may be missing"));
        if !notes.is_empty() {
            f.lines.extend(prose(&p.warn, &notes.join(" · "), budget, " "));
        }
    }
}

/// One property's row figures: active now, change against baseline,
/// sessions today against last week, key events against last week.
fn property_cells(prop: &Prop, v: &View) -> (String, String, String, String, String) {
    let tz = model::tz_of(&prop.time_zone);
    let now = prop.rt.as_ref().map(|r| model::thousands(r.active)).unwrap_or_else(|| "–".into());
    let base = match (&prop.rt, model::baseline(&prop.samples, v.now, v.weeks, tz, &|s| s.active)) {
        (Some(r), Baseline::Ready { median, .. }) => model::pct(r.active, median).map(model::pct_label).unwrap_or_else(|| "–".into()),
        (_, Baseline::Collecting { .. }) => "collecting".into(),
        (_, Baseline::Uncovered { .. }) => "no base".into(),
        _ => "–".into(),
    };
    let (today, key) = match &prop.processed {
        // Reports asked for before the property's midnight are
        // yesterday's; until the next refresh, today has nothing yet.
        Some(proc_) if proc_.date != model::local_now(v.now, tz).0 => ("not yet".into(), "not yet".into()),
        Some(proc_) => {
            let h = model::hourly(&proc_.hourly, "sessions");
            match model::versus(&h) {
                Some(vs) => {
                    let t = vs.last.and_then(|l| model::pct(vs.today, l)).map(model::pct_label).unwrap_or_else(|| "–".into());
                    let date = proc_.date;
                    let names = if v.configured.is_empty() { prop.key_names.clone().unwrap_or_default() } else { v.configured.clone() };
                    let metric = if v.configured.is_empty() { "keyEvents" } else { "eventCount" };
                    let lines = model::key_events(&proc_.key_events, &names, date, Some(vs.upto), metric);
                    let (a, b) = lines.iter().fold((0.0, 0.0), |acc, l| (acc.0 + l.today, acc.1 + l.last));
                    let k = if names.is_empty() { "none set".into() } else { model::pct(a, b).map(model::pct_label).unwrap_or_else(|| "–".into()) };
                    (t, k)
                }
                None => ("not yet".into(), "not yet".into()),
            }
        }
        None => ("–".into(), "–".into()),
    };
    let state = if !prop.died.is_empty() {
        "stopped".to_string()
    } else if !prop.rt_banner.is_empty() || !prop.core_banner.is_empty() {
        "429".into()
    } else if !prop.rt_err.is_empty() || !prop.bd_err.is_empty() || !prop.core_err.is_empty() || !prop.admin_err.is_empty() {
        "error".into()
    } else if !prop.interval_why.is_empty() {
        format!("{}s", prop.interval)
    } else if prop.rt.is_some() {
        "ok".into()
    } else {
        "waiting".into()
    };
    (now, base, today, key, state)
}

fn properties(f: &mut Frame, shared: &Shared, v: &View, budget: usize, p: &Palette) {
    f.lines.extend(heading(p, "PROPERTIES", "active now · vs baseline · sessions and key events today vs same weekday", budget));
    let wide = budget >= 70;
    let (nw, bw, tw, kw, sw) = (6usize, 11usize, 9usize, 9usize, 9usize);
    let fixed = nw + bw + if wide { tw + kw + sw } else { 0 };
    let name_w = budget.saturating_sub(1 + fixed);
    let mut head = vec![right("NOW", nw), right("VS BASE", bw)];
    if wide {
        head.push(right("TODAY", tw));
        head.push(right("KEY EV", kw));
        head.push(right("STATE", sw));
    }
    f.lines.extend(header(p, "PROPERTY", name_w, head, budget));
    for (i, prop) in shared.props.iter().enumerate() {
        let (now, base, today, key, state) = property_cells(prop, v);
        let mut cells = vec![(p.txt.clone(), right(&now, nw)), (p.dim.clone(), right(&base, bw))];
        if wide {
            cells.push((p.dim.clone(), right(&today, tw)));
            cells.push((p.dim.clone(), right(&key, kw)));
            let sc = if state == "ok" { p.good.clone() } else if state == "waiting" { p.dim.clone() } else { p.warn.clone() };
            cells.push((sc, right(&state, sw)));
        }
        let name = format!("{} ({})", if prop.name.is_empty() { "unnamed" } else { &prop.name }, prop.id);
        let colour = if i == v.prop { p.accent.clone() } else { p.txt.clone() };
        push_item(f, v, Item::Property(i), |here| table_row(p, true, here, &name, &colour, name_w, &cells, budget));
    }
}

/// The detail screen for a property: its quota in full, its settings as
/// GA reports them, and the baseline file.
pub fn property_detail(prop: &Prop, v: &View, w: usize) -> Vec<Line> {
    let p = palette();
    let budget = w.saturating_sub(1).max(1);
    let mut out: Vec<Line> = Vec::new();
    out.extend(prose(&p.txt, &format!("{} (properties/{})", prop.name, prop.id), budget, " "));
    out.push(Vec::new());
    let tz = if prop.time_zone.is_empty() { "not read yet".to_string() } else { prop.time_zone.clone() };
    let cur = if prop.currency.is_empty() { "not read yet".to_string() } else { prop.currency.clone() };
    out.extend(flow(
        vec![vec![part(&p.dim, "timezone "), part(&p.txt, tz)], vec![part(&p.dim, "currency "), part(&p.txt, cur)]],
        budget,
        "   ",
    ));
    let mut flags: Vec<String> = Vec::new();
    if let Some(proc_) = &prop.processed {
        for (name, r) in [
            ("hourly", &proc_.hourly),
            ("sources today", &proc_.sources),
            ("key events", &proc_.key_events),
            ("pages by day", &proc_.pages),
            ("sources by window", &proc_.source_movers),
        ]
        .into_iter()
        .chain(
            [("page totals", &proc_.page_totals), ("channels", &proc_.channels), ("organic", &proc_.organic)]
                .into_iter()
                .filter_map(|(n, r)| r.as_ref().map(|r| (n, r))),
        ) {
            let mut n: Vec<String> = meta_note(r).into_iter().map(str::to_string).collect();
            if r.row_count > r.rows.len() {
                n.push(format!("cut at {} of {} rows", r.rows.len(), r.row_count));
            }
            flags.push(format!("{}: {}", name, if n.is_empty() { "complete, not thresholded or sampled".into() } else { n.join(", ") }));
        }
    } else {
        flags.push("no processed reports yet".into());
    }
    out.push(Vec::new());
    out.extend(heading(&p, "DATA QUALITY", "as GA reported it on the last processed reports", budget));
    for line in flags {
        out.extend(prose(&p.dim, &line, budget, " "));
    }
    for (title, q) in [("QUOTA: REALTIME", &prop.quota_rt), ("QUOTA: CORE REPORTS", &prop.quota_core)] {
        out.push(Vec::new());
        out.extend(heading(
            &p,
            title,
            "from propertyQuota on the last response · used = Google's documented limit less what is left",
            budget,
        ));
        match q {
            None => out.extend(prose(&p.dim, "no response read yet", budget, " ")),
            Some(q) => {
                let name_w = budget.saturating_sub(1 + 40).max(10);
                let wide = budget >= 1 + 40 + 10;
                if wide {
                    out.push(vec![
                        part(&p.head, format!(" {}", left("BUCKET", name_w))),
                        part(&p.head, right("USED", 10)),
                        part(&p.head, right("LEFT", 10)),
                        part(&p.head, right("LIMIT", 10)),
                        part(&p.head, right("LAST REQ", 10)),
                    ]);
                }
                for (name, b) in &q.buckets {
                    let level = q.level(name, prop.premium);
                    let cell = |x: Option<f64>| x.map(model::thousands).unwrap_or_else(|| "–".into());
                    if wide {
                        out.extend(table_row(
                            &p,
                            false,
                            false,
                            name,
                            &p.txt,
                            name_w,
                            &[
                                (p.txt.clone(), right(&cell(level.map(|l| l.used)), 10)),
                                (p.dim.clone(), right(&model::thousands(b.remaining), 10)),
                                (p.dim.clone(), right(&cell(level.map(|l| l.limit)), 10)),
                                (p.dim.clone(), right(&model::thousands(b.consumed), 10)),
                            ],
                            budget,
                        ));
                    } else {
                        let said = match level {
                            Some(l) => format!(
                                "{}: {} used of {}, {} left, last request {}",
                                name,
                                model::thousands(l.used),
                                model::thousands(l.limit),
                                model::thousands(l.left),
                                model::thousands(l.last)
                            ),
                            None => format!("{}: {} left, last request {}", name, model::thousands(b.remaining), model::thousands(b.consumed)),
                        };
                        out.extend(prose(&p.txt, &said, budget, " "));
                    }
                }
            }
        }
    }
    out.push(Vec::new());
    out.extend(heading(&p, "BASELINE FILE", "numbers this widget measured itself", budget));
    let path = if prop.sample_path.is_empty() { "not opened yet".to_string() } else { prop.sample_path.clone() };
    out.extend(prose(&p.txt, &path, budget, " "));
    let held = match (prop.samples.first(), prop.samples.last()) {
        (Some(a), Some(b)) => format!(
            "{} samples over {:.1} weeks; the baseline asks for {} weeks",
            prop.samples.len(),
            (b.t - a.t) / 604_800.0,
            v.weeks
        ),
        _ => format!("no samples yet; the baseline asks for {} weeks", v.weeks),
    };
    out.extend(prose(&p.dim, &held, budget, " "));
    out.extend(prose(
        &p.dim,
        &format!("realtime every {}s{}", prop.interval.max(1), if prop.interval_why.is_empty() { String::new() } else { format!(" ({})", prop.interval_why) }),
        budget,
        " ",
    ));
    out
}

fn spark_line(p: &Palette, label: &str, values: &[f64], colour: &str, budget: usize) -> Vec<Line> {
    let hi = values.iter().cloned().fold(0.0, f64::max);
    let drawn = bar(&values.iter().map(|x| Some(*x)).collect::<Vec<_>>(), hi);
    let lw = 12usize;
    if lw + values.len() <= budget {
        vec![vec![part(&p.dim, left(&format!(" {}", label), lw)), part(colour, drawn)]]
    } else {
        let mut out = vec![vec![part(&p.dim, format!(" {}", label))]];
        let room = budget.saturating_sub(1).max(1);
        let chars: Vec<char> = drawn.chars().collect();
        for chunk in chars.chunks(room) {
            out.push(vec![part(colour, format!(" {}", chunk.iter().collect::<String>()))]);
        }
        out
    }
}

fn small_table(p: &Palette, title: &str, r: &Report, dim: &str, metric: &str, budget: usize) -> Vec<Line> {
    let mut out = vec![vec![part(&p.head, format!(" {}", title))]];
    let rows = model::top(r, dim, metric, 10);
    if rows.is_empty() {
        let why = if r.meta.empty_reason.is_empty() { "none in this window".to_string() } else { format!("GA: {}", r.meta.empty_reason) };
        out.extend(prose(&p.dim, &why, budget, "   "));
        return out;
    }
    let name_w = budget.saturating_sub(1 + 10);
    for (name, n) in rows {
        let name = if name.is_empty() { "(not set)".to_string() } else { name };
        out.extend(table_row(p, false, false, &name, &p.txt, name_w, &[(p.txt.clone(), right(&model::thousands(n), 10))], budget));
    }
    out
}

/// The detail for a page: views and users by day over 28 days with the
/// 28 before drawn under them, then where its visitors came from.
pub fn page_detail(path: &str, reports: Option<&Result<Vec<Report>, String>>, w: usize) -> Vec<Line> {
    let p = palette();
    let budget = w.saturating_sub(1).max(1);
    let mut out = prose(&p.txt, path, budget, " ");
    out.push(Vec::new());
    let reports = match reports {
        None => {
            out.extend(prose(&p.dim, "asking GA for this page...", budget, " "));
            return out;
        }
        Some(Err(e)) => {
            out.extend(prose(&p.bad, e, budget, " ! "));
            return out;
        }
        Some(Ok(r)) if r.len() < 5 => {
            out.extend(prose(&p.bad, &format!("GA answered {} of the 5 reports asked for", r.len()), budget, " ! "));
            return out;
        }
        Some(Ok(r)) => r,
    };
    out.extend(heading(&p, "28 DAYS", "by day, oldest first · the prior 28 days under it", budget));
    for (metric, word) in [("screenPageViews", "views"), ("totalUsers", "users")] {
        let mut cur: Vec<(String, f64)> = Vec::new();
        let mut prev: Vec<(String, f64)> = Vec::new();
        for row in &reports[0].rows {
            let pair = (reports[0].d(row, "date").to_string(), reports[0].m(row, metric));
            match reports[0].d(row, "dateRange") {
                "prev" => prev.push(pair),
                _ => cur.push(pair),
            }
        }
        cur.sort_by(|a, b| a.0.cmp(&b.0));
        prev.sort_by(|a, b| a.0.cmp(&b.0));
        let c: Vec<f64> = cur.iter().map(|x| x.1).collect();
        let pv: Vec<f64> = prev.iter().map(|x| x.1).collect();
        let (sc, sp): (f64, f64) = (c.iter().sum(), pv.iter().sum());
        out.extend(spark_line(&p, word, &c, &p.good, budget));
        out.extend(spark_line(&p, "prior", &pv, &p.dim, budget));
        let d = model::pct(sc, sp);
        out.extend(prose(
            &p.dim,
            &format!("{} 28d {} vs prior {} ({})", word, model::thousands(sc), model::thousands(sp), d.map(model::pct_label).unwrap_or_else(|| "no prior to compare".into())),
            budget,
            " ",
        ));
    }
    out.push(Vec::new());
    out.extend(small_table(&p, "SOURCES (views, 28d)", &reports[1], "sessionSourceMedium", "screenPageViews", budget));
    out.extend(small_table(&p, "COUNTRIES (views, 28d)", &reports[2], "country", "screenPageViews", budget));
    out.extend(small_table(&p, "DEVICES (views, 28d)", &reports[3], "deviceCategory", "screenPageViews", budget));
    out.extend(small_table(&p, "KEY EVENTS ON THIS PAGE (28d)", &reports[4], "eventName", "keyEvents", budget));
    out.push(Vec::new());
    out.extend(prose(
        &p.dim,
        "live: GA realtime names pages by title, not path, so this page is not matched to a live count - see RIGHT NOW for live pages as GA reports them",
        budget,
        " ",
    ));
    out
}

/// The detail for a key event: by day, by source today, by page.
pub fn event_detail(name: &str, reports: Option<&Result<Vec<Report>, String>>, metric: &str, w: usize) -> Vec<Line> {
    let p = palette();
    let budget = w.saturating_sub(1).max(1);
    let mut out = prose(&p.txt, name, budget, " ");
    out.push(Vec::new());
    let reports = match reports {
        None => {
            out.extend(prose(&p.dim, "asking GA for this event...", budget, " "));
            return out;
        }
        Some(Err(e)) => {
            out.extend(prose(&p.bad, e, budget, " ! "));
            return out;
        }
        Some(Ok(r)) if r.len() < 3 => {
            out.extend(prose(&p.bad, &format!("GA answered {} of the 3 reports asked for", r.len()), budget, " ! "));
            return out;
        }
        Some(Ok(r)) => r,
    };
    out.extend(heading(&p, "BY DAY", "28 days, oldest first · today is still being processed", budget));
    let mut days: Vec<(String, f64)> = reports[0].rows.iter().map(|r| (reports[0].d(r, "date").to_string(), reports[0].m(r, metric))).collect();
    days.sort_by(|a, b| a.0.cmp(&b.0));
    let vals: Vec<f64> = days.iter().map(|d| d.1).collect();
    if vals.is_empty() {
        out.extend(prose(&p.dim, "none in 28 days", budget, " "));
    } else {
        out.extend(spark_line(&p, "per day", &vals, &p.good, budget));
        out.extend(prose(&p.dim, &format!("{} in 28 days", model::thousands(vals.iter().sum())), budget, " "));
    }
    out.push(Vec::new());
    out.extend(small_table(&p, "BY SOURCE TODAY", &reports[1], "sessionSourceMedium", metric, budget));
    out.extend(small_table(&p, "BY PAGE (28d)", &reports[2], "pagePath", metric, budget));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::{parse_batch, parse_report, Bucket, Quota, Sample};
    use crate::poll::{Breakdown, Processed, Rt};

    const NOW: f64 = 1_792_506_900.0;
    const WEEK: f64 = 604_800.0;

    fn rep(text: &str) -> Report {
        parse_report(text).unwrap()
    }

    fn long_path() -> String {
        "/docs/a-very-long-section-name/with-a-deeply-nested-page-that-keeps-going-well-past-any-column".into()
    }

    fn full_prop() -> Prop {
        let hourly = rep(r#"{"dimensionHeaders":[{"name":"hour"},{"name":"dateRange"}],"metricHeaders":[{"name":"sessions"}],"rows":[
            {"dimensionValues":[{"value":"00"},{"value":"today"}],"metricValues":[{"value":"40"}]},
            {"dimensionValues":[{"value":"09"},{"value":"today"}],"metricValues":[{"value":"1200"}]},
            {"dimensionValues":[{"value":"00"},{"value":"w1"}],"metricValues":[{"value":"30"}]},
            {"dimensionValues":[{"value":"09"},{"value":"w1"}],"metricValues":[{"value":"1100"}]},
            {"dimensionValues":[{"value":"22"},{"value":"w1"}],"metricValues":[{"value":"90"}]},
            {"dimensionValues":[{"value":"09"},{"value":"w2"}],"metricValues":[{"value":"900"}]}]}"#);
        let sources = rep(r#"{"dimensionHeaders":[{"name":"sessionDefaultChannelGroup"},{"name":"sessionSourceMedium"},{"name":"hour"},{"name":"dateRange"}],"metricHeaders":[{"name":"sessions"}],"rows":[
            {"dimensionValues":[{"value":"Organic Search"},{"value":"google / organic"},{"value":"09"},{"value":"today"}],"metricValues":[{"value":"1120"}]},
            {"dimensionValues":[{"value":"Email"},{"value":"newsletter / email-with-a-long-campaign-name"},{"value":"09"},{"value":"w1"}],"metricValues":[{"value":"86"}]}]}"#);
        let key_events = rep(r#"{"dimensionHeaders":[{"name":"date"},{"name":"hour"},{"name":"eventName"}],"metricHeaders":[{"name":"keyEvents"},{"name":"eventCount"}],"rows":[
            {"dimensionValues":[{"value":"20261020"},{"value":"09"},{"value":"purchase"}],"metricValues":[{"value":"23"},{"value":"23"}]},
            {"dimensionValues":[{"value":"20261013"},{"value":"09"},{"value":"purchase"}],"metricValues":[{"value":"31"},{"value":"31"}]},
            {"dimensionValues":[{"value":"20261013"},{"value":"09"},{"value":"a_key_event_with_an_unusually_long_name_for_testing"}],"metricValues":[{"value":"5"},{"value":"5"}]}]}"#);
        let lp = long_path();
        let pages = rep(&format!(r#"{{"dimensionHeaders":[{{"name":"pagePath"}},{{"name":"date"}},{{"name":"dateRange"}}],"metricHeaders":[{{"name":"screenPageViews"}},{{"name":"totalUsers"}},{{"name":"sessions"}}],"rowCount":9999,"metadata":{{"subjectToThresholding":true,"dataLossFromOtherRow":true}},"rows":[
            {{"dimensionValues":[{{"value":"/pricing"}},{{"value":"20261019"}},{{"value":"cur"}}],"metricValues":[{{"value":"9120"}},{{"value":"800"}},{{"value":"700"}}]}},
            {{"dimensionValues":[{{"value":"/pricing"}},{{"value":"20261010"}},{{"value":"prev"}}],"metricValues":[{{"value":"6480"}},{{"value":"600"}},{{"value":"500"}}]}},
            {{"dimensionValues":[{{"value":"{}"}},{{"value":"20261019"}},{{"value":"cur"}}],"metricValues":[{{"value":"890"}},{{"value":"80"}},{{"value":"60"}}]}}]}}"#, lp));
        let page_totals = rep(&format!(r#"{{"dimensionHeaders":[{{"name":"pagePath"}},{{"name":"dateRange"}}],"metricHeaders":[{{"name":"screenPageViews"}},{{"name":"totalUsers"}},{{"name":"sessions"}}],"metadata":{{"subjectToThresholding":true,"dataLossFromOtherRow":true}},"rows":[
            {{"dimensionValues":[{{"value":"/pricing"}},{{"value":"cur"}}],"metricValues":[{{"value":"9120"}},{{"value":"700"}},{{"value":"700"}}]}},
            {{"dimensionValues":[{{"value":"/pricing"}},{{"value":"prev"}}],"metricValues":[{{"value":"6480"}},{{"value":"550"}},{{"value":"500"}}]}},
            {{"dimensionValues":[{{"value":"{}"}},{{"value":"cur"}}],"metricValues":[{{"value":"890"}},{{"value":"80"}},{{"value":"60"}}]}}]}}"#, long_path()));
        let channels = rep(r#"{"dimensionHeaders":[{"name":"sessionDefaultChannelGroup"},{"name":"sessionSourceMedium"},{"name":"dateRange"}],"metricHeaders":[{"name":"screenPageViews"},{"name":"totalUsers"},{"name":"sessions"}],"rows":[
            {"dimensionValues":[{"value":"Organic Search"},{"value":"google / organic"},{"value":"cur"}],"metricValues":[{"value":"5000"},{"value":"400"},{"value":"300"}]},
            {"dimensionValues":[{"value":"Direct"},{"value":"(direct) / (none)"},{"value":"prev"}],"metricValues":[{"value":"100"},{"value":"40"},{"value":"30"}]}]}"#);
        let organic = rep(r#"{"dimensionHeaders":[{"name":"landingPage"},{"name":"dateRange"}],"metricHeaders":[{"name":"sessions"},{"name":"keyEvents"}],"rows":[
            {"dimensionValues":[{"value":"/blog/launch?utm_source=a-very-long-query-string-value"},{"value":"cur"}],"metricValues":[{"value":"400"},{"value":"12"}]},
            {"dimensionValues":[{"value":"/blog/launch?utm_source=a-very-long-query-string-value"},{"value":"prev"}],"metricValues":[{"value":"300"},{"value":"10"}]}]}"#);
        let realtime_pages = rep(r#"{"dimensionHeaders":[{"name":"unifiedScreenName"}],"metricHeaders":[{"name":"activeUsers"}],"rows":[
            {"dimensionValues":[{"value":"Pricing - Example Site, the page title that runs on and on"}],"metricValues":[{"value":"34"}]},
            {"dimensionValues":[{"value":"Home"}],"metricValues":[{"value":"19"}]}]}"#);
        let devices = rep(r#"{"dimensionHeaders":[{"name":"deviceCategory"}],"metricHeaders":[{"name":"activeUsers"}],"rows":[{"dimensionValues":[{"value":"mobile"}],"metricValues":[{"value":"52"}]}]}"#);
        let countries = rep(r#"{"dimensionHeaders":[{"name":"country"}],"metricHeaders":[{"name":"activeUsers"}],"rows":[{"dimensionValues":[{"value":"United States"}],"metricValues":[{"value":"41"}]}]}"#);
        let events = rep(r#"{"dimensionHeaders":[{"name":"eventName"}],"metricHeaders":[{"name":"eventCount"},{"name":"keyEvents"}],"rows":[{"dimensionValues":[{"value":"purchase"}],"metricValues":[{"value":"3"},{"value":"3"}]}]}"#);
        let quota = Quota {
            buckets: vec![
                ("tokensPerDay".into(), Bucket { consumed: 30.0, remaining: 169_000.0 }),
                ("tokensPerHour".into(), Bucket { consumed: 30.0, remaining: 38_800.0 }),
                ("tokensPerProjectPerHour".into(), Bucket { consumed: 30.0, remaining: 12_800.0 }),
                ("concurrentRequests".into(), Bucket { consumed: 0.0, remaining: 10.0 }),
                ("potentiallyThresholdedRequestsPerHour".into(), Bucket { consumed: 1.0, remaining: 119.0 }),
            ],
        };
        Prop {
            id: "123".into(),
            name: "Example Site".into(),
            time_zone: "UTC".into(),
            currency: "USD".into(),
            key_names: Some(vec!["purchase".into(), "sign_up".into(), "a_key_event_with_an_unusually_long_name_for_testing".into()]),
            rt: Some(Rt { active: 71.0, views: 540.0, key: 3.0, minutes: (0..30).map(|i| (i % 7) as f64).collect(), views_minutes: vec![18.0; 30] }),
            rt_at: NOW - 9.0,
            breakdown: Some(Breakdown { pages: realtime_pages, devices, countries, events }),
            breakdown_at: NOW - 30.0,
            processed: Some(Processed {
                hourly,
                sources,
                key_events,
                pages,
                source_movers: channels.clone(),
                page_totals: Some(page_totals),
                channels: Some(channels),
                organic: Some(organic),
                window_at: NOW - 180.0,
                window: 7,
                date: chrono::NaiveDate::from_ymd_opt(2026, 10, 20).unwrap(),
            }),
            core_at: NOW - 180.0,
            quota_rt: Some(quota.clone()),
            quota_core: Some(quota),
            interval: 60,
            interval_why: "backed off: 0 active for 5m+".into(),
            samples: vec![
                Sample { t: NOW - 2.0 * WEEK, active: Some(48.0), views: Some(1.0), events: Some(vec![("purchase".into(), 2.0)]) },
                Sample { t: NOW - WEEK, active: Some(50.0), views: Some(1.0), events: Some(vec![]) },
            ],
            sample_path: "/home/someone/.local/state/opscope/google-analytics/123.samples".into(),
            rt_err: "403 no access: add reader@example-project.iam.gserviceaccount.com as Viewer under Property access management · Google said: User does not have sufficient permissions for this property.".into(),
            rt_banner: "429 quota: property tokens per hour exhausted (hourly bucket, refills within the hour)".into(),
            ..Prop::default()
        }
    }

    fn shared(props: Vec<Prop>) -> Shared {
        Shared { props, chosen: 15, window: 7, ..Shared::default() }
    }

    fn view() -> View {
        View {
            prop: 0,
            metric: 0,
            sort: Sort::Change,
            needle: String::new(),
            typing: false,
            selected: 0,
            configured: Vec::new(),
            weeks: 4,
            cred_note: Some("token from $GOOGLE_OAUTH_ACCESS_TOKEN · Google tokens last 1h".into()),
            now: NOW,
        }
    }

    fn text(lines: &[Line]) -> String {
        lines.iter().map(plain).collect::<Vec<_>>().join("\n")
    }

    /// The same text with every line's edges and wrapping taken out, for
    /// asserting that something long arrived whole.
    fn joined(lines: &[Line]) -> String {
        lines.iter().map(|l| plain(l).trim().to_string()).collect::<Vec<_>>().join("")
    }

    #[test]
    fn every_row_fits_at_every_width() {
        let mut second = full_prop();
        second.id = "456".into();
        second.name = "Example Docs with a long property name".into();
        let s = shared(vec![full_prop(), second]);
        for w in [30usize, 40, 60, 100, 140] {
            for selected in [0usize, 3, 7] {
                let mut v = view();
                v.selected = selected;
                let f = body(&s, &v, w);
                for line in &f.lines {
                    assert!(width_of(line) <= w - 1, "{} cols: {:?} is {} wide", w, plain(line), width_of(line));
                }
                let details = [
                    property_detail(&s.props[0], &v, w),
                    page_detail("/pricing", None, w),
                    event_detail("purchase", Some(&Err("HTTP 500".into())), "keyEvents", w),
                ];
                for d in details.iter().flatten() {
                    assert!(width_of(d) <= w - 1, "{} cols: {:?}", w, plain(d));
                }
            }
        }
    }

    #[test]
    fn a_long_name_wraps_rather_than_losing_its_end() {
        let p = palette();
        let name = long_path();
        for name_w in [6usize, 14, 40] {
            let cells = vec![(p.txt.clone(), right("9,120", 9)), (p.good.clone(), left("  +41%", 9))];
            let lines = table_row(&p, true, true, &name, &p.txt, name_w, &cells, name_w + 1 + 18);
            let rebuilt: String = lines
                .iter()
                .map(|l| {
                    let t: String = plain(l).chars().skip(1).collect();
                    t.replace(&cells[0].1, "").replace(&cells[1].1, "").trim().to_string()
                })
                .collect();
            assert_eq!(rebuilt, name, "at a {}-wide name column", name_w);
            let at = if name_w < 8 { lines.len() - 1 } else { 0 };
            assert!(plain(&lines[at]).contains("9,120"), "the figures sit beside the name, or under it when narrow");
            for l in &lines {
                assert!(width_of(l) <= name_w + 1 + 18);
            }
        }
        // Prose wraps at words and loses none of them.
        let s = shared(vec![full_prop()]);
        for w in [30usize, 40, 60, 100, 140] {
            let all = text(&body(&s, &view(), w).lines);
            let words: Vec<&str> = all.split_whitespace().collect();
            let flat = words.join(" ");
            assert!(flat.contains("as Viewer under Property access management"), "{} cols", w);
            assert!(flat.contains("the page each lands on."), "{} cols", w);
        }
    }

    #[test]
    fn the_sections_say_what_they_measure() {
        let s = shared(vec![full_prop()]);
        let all = text(&body(&s, &view(), 140).lines);
        for want in [
            "── RIGHT NOW ── last 30 min · GA realtime",
            "── TODAY VS SAME WEEKDAY ── sessions by hour",
            "── KEY EVENTS ── today to 10:00 vs same weekday",
            "── TOP ORGANIC LANDING PAGES ──",
            "GA4 does not give the search terms",
            "google-search-console",
            "── MOVERS ── views · last 7 days vs prior 7",
            "sources: not in GA realtime",
            "live 60s (backed off: 0 active for 5m+)",
            "quota realtime used 1.2k/14k h 31k/200k d",
            "token from $GOOGLE_OAUTH_ACCESS_TOKEN · Google tokens last 1h",
            "429 quota: property tokens per hour exhausted",
            "realtime paused 5m, last reading kept below",
            "active users 71",
            "baseline 49 · from 2 of 4 weeks · collecting since",
            "GA grouped some rows into (other) · GA withheld rows below its privacy thresholds",
            "GA returned 3 of 9999 page-day rows",
        ] {
            assert!(all.contains(want), "missing {:?} in\n{}", want, all);
        }
    }

    #[test]
    fn unprocessed_hours_are_hatched_and_quiet_ones_are_not() {
        let h = model::hourly(&full_prop().processed.unwrap().hourly, "sessions");
        let drawn = bar(&h.today, 1200.0);
        assert_eq!(drawn.chars().count(), 24);
        let chars: Vec<char> = drawn.chars().collect();
        assert_ne!(chars[0], '░');
        assert_eq!(chars[1], '·', "a processed hour with no sessions is a zero, drawn as one");
        assert_eq!(chars[9], '█');
        assert!(chars[10..].iter().all(|c| *c == '░'), "{}", drawn);
        assert_eq!(bar(&[Some(0.0), None], 1.0), "·░");
    }

    #[test]
    fn baseline_wording_names_its_three_states() {
        let tz = chrono_tz::UTC;
        assert_eq!(baseline_words(&Baseline::Collecting { since: NOW - 86_400.0 }, 3.0, tz).0, "baseline: collecting since Oct 19");
        assert_eq!(
            baseline_words(&Baseline::Uncovered { label: "Tue 14:35".into() }, 3.0, tz).0,
            "no baseline for Tue 14:35 yet - the widget was not running then"
        );
        let (s, d) = baseline_words(&Baseline::Ready { median: 48.0, found: 2, of: 4, since: None }, 71.0, tz);
        assert_eq!(s, "baseline 48 · from 2 of 4 weeks");
        assert_eq!(d.map(model::pct_label).as_deref(), Some("+48%"));
    }

    #[test]
    fn zero_active_is_drawn_as_zero_beside_its_baseline() {
        let mut prop = full_prop();
        prop.rt.as_mut().unwrap().active = 0.0;
        let all = text(&body(&shared(vec![prop]), &view(), 100).lines);
        assert!(all.contains("active users 0   baseline 49 · from 2 of 4 weeks"), "{}", all);
        assert!(all.contains("\u{2212}100%"));
    }

    #[test]
    fn no_realtime_baseline_falls_back_to_hourly_processed_data() {
        let mut prop = full_prop();
        prop.samples.clear();
        let all = text(&body(&shared(vec![prop]), &view(), 140).lines);
        assert!(all.contains("baseline: collecting since"));
        assert!(all.contains("hourly instead: sessions 09:00-09:59 today 1,200 vs last Tue 1,100 (+9%) · processed hourly data, not realtime"), "{}", all);
    }

    #[test]
    fn key_event_states_are_worded_apart() {
        // No key events at all.
        let mut prop = full_prop();
        prop.key_names = Some(Vec::new());
        let all = text(&body(&shared(vec![prop]), &view(), 100).lines);
        assert!(all.contains("no key events configured"), "{}", all);
        // Zero today against a last week that had some.
        let prop = full_prop();
        let all = text(&body(&shared(vec![prop]), &view(), 100).lines);
        assert!(all.contains("none today"), "{}", all);
        // Nothing of today processed yet.
        let mut prop = full_prop();
        let hourly = rep(r#"{"dimensionHeaders":[{"name":"hour"},{"name":"dateRange"}],"metricHeaders":[{"name":"sessions"}],"rows":[
            {"dimensionValues":[{"value":"09"},{"value":"w1"}],"metricValues":[{"value":"1100"}]}]}"#);
        prop.processed.as_mut().unwrap().hourly = hourly;
        let all = text(&body(&shared(vec![prop]), &view(), 100).lines);
        assert!(all.contains("not processed"), "{}", all);
        assert!(all.contains("today: no hour processed yet"), "{}", all);
        assert!(!all.contains("none today"));
    }

    #[test]
    fn reports_from_before_midnight_are_not_called_today() {
        let mut prop = full_prop();
        prop.processed.as_mut().unwrap().date = chrono::NaiveDate::from_ymd_opt(2026, 10, 19).unwrap();
        let s = shared(vec![prop.clone(), prop]);
        let all = text(&body(&s, &view(), 100).lines);
        assert!(all.contains("these are Mon's reports, asked for before midnight"), "{}", all);
        assert!(all.contains("last Mon"), "the comparison names the reports' weekday, not today's");
        assert!(!all.contains("hourly instead"), "no fallback built from yesterday's hours");
    }

    #[test]
    fn a_failed_window_refresh_keeps_the_last_answer_and_says_how_old() {
        let mut prop = full_prop();
        prop.processed.as_mut().unwrap().window_at = NOW - 900.0;
        prop.core_err = "page and channel totals, organic landing pages: could not reach GA: timed out".into();
        let mut v = view();
        v.configured = vec!["purchase".into()];
        let all = text(&body(&shared(vec![prop]), &v, 100).lines);
        assert!(all.contains("window totals and organic pages read "), "{}", all);
        assert!(all.contains("the latest refresh of them failed"), "{}", all);
        assert!(all.contains("/pricing"), "the last answer is still drawn");
        assert!(all.contains("KEY EV here counts the events GA flags"), "{}", all);
    }

    #[test]
    fn every_failure_has_a_named_row() {
        let mut prop = full_prop();
        prop.rt = None;
        prop.processed = None;
        prop.breakdown = None;
        prop.died = "poller stopped: index out of bounds".into();
        prop.core_err = "API not enabled: turn on analyticsdata.googleapis.com in the key's Cloud project (APIs & Services)".into();
        prop.admin_err = "Google refused the sign-in: invalid_grant - Invalid JWT Signature.".into();
        prop.core_banner = "429 quota: property tokens per day exhausted (daily bucket, resets at midnight Pacific time)".into();
        prop.sample_err = "could not write the sample file /x: permission denied".into();
        prop.bd_err = "realtime breakdowns: could not reach GA: timed out".into();
        let mut s = shared(vec![prop]);
        s.discovery_err = "no GA4 properties visible to reader@example-project.iam.gserviceaccount.com".into();
        let mut v = view();
        v.cred_note = Some("~/key.json is readable by others; chmod 600 it".into());
        let all = text(&body(&s, &v, 100).lines);
        for want in [
            "poller stopped: index out of bounds",
            "API not enabled",
            "invalid_grant",
            "403 no access",
            "reports paused 5m",
            "daily bucket",
            "could not write the sample file",
            "realtime breakdowns: could not reach GA",
            "no GA4 properties visible",
            "chmod 600",
            "no realtime reading - the reason is above",
            "no processed reports - the reason is above",
        ] {
            assert!(all.contains(want), "missing {:?}\n{}", want, all);
        }
    }

    #[test]
    fn an_empty_report_shows_why() {
        let mut prop = full_prop();
        let proc_ = prop.processed.as_mut().unwrap();
        proc_.organic = Some(rep(r#"{"dimensionHeaders":[{"name":"landingPage"},{"name":"dateRange"}],"metricHeaders":[{"name":"sessions"}],"metadata":{"emptyReason":"No Google Search sessions matched."}}"#));
        let all = text(&body(&shared(vec![prop]), &view(), 100).lines);
        assert!(all.contains("GA: No Google Search sessions matched."), "{}", all);
    }

    #[test]
    fn width_buys_columns_not_padding() {
        let s = shared(vec![full_prop()]);
        let narrow = text(&body(&s, &view(), 60).lines);
        let mid = text(&body(&s, &view(), 100).lines);
        let wide = text(&body(&s, &view(), 140).lines);
        assert!(!narrow.contains("PRIOR"), "movers drop PRIOR under 70");
        assert!(mid.contains("PRIOR"));
        assert!(!mid.contains("LAST 14 DAYS"));
        assert!(wide.contains("LAST 14 DAYS"), "above 120 the movers gain a sparkline");
        // Side by side above 70: the three breakdown titles on one row.
        assert!(mid.lines().any(|l| l.contains("PAGE (title)") && l.contains("DEVICE") && l.contains("COUNTRY")));
        assert!(!narrow.lines().any(|l| l.contains("PAGE (title)") && l.contains("DEVICE")));
    }

    #[test]
    fn every_item_is_placed_where_it_was_drawn() {
        let mut second = full_prop();
        second.id = "456".into();
        let s = shared(vec![full_prop(), second]);
        let f = body(&s, &view(), 100);
        assert!(f.items.iter().any(|i| matches!(i, Item::Event(_))));
        assert!(f.items.iter().any(|i| matches!(i, Item::Organic(_))));
        assert!(f.items.iter().any(|i| matches!(i, Item::Page(_))));
        assert!(f.items.iter().any(|i| matches!(i, Item::Channel(_))));
        assert!(f.items.iter().any(|i| matches!(i, Item::Property(1))));
        for (row, item) in &f.placed {
            assert!(*item < f.items.len());
            assert!(*row < f.lines.len());
        }
        let mut v = view();
        v.selected = f.items.len() - 1;
        let f = body(&s, &v, 100);
        let c = f.cursor.unwrap();
        assert!(plain(&f.lines[c]).starts_with('▸') || f.placed.iter().any(|(r, i)| *r == c && *i == v.selected));
    }

    #[test]
    fn a_filter_is_named_with_its_count() {
        let s = shared(vec![full_prop()]);
        let mut v = view();
        v.needle = "pricing".into();
        let all = text(&body(&s, &v, 100).lines);
        assert!(all.contains("shown · /pricing"), "{}", all);
        v.needle = "nothing-matches-this".into();
        let all = text(&body(&s, &v, 100).lines);
        assert!(all.contains("nothing matches · all"), "{}", all);
    }

    #[test]
    fn every_colour_clears_aa_on_the_tint() {
        fn lum(c: (f64, f64, f64)) -> f64 {
            let f = |x: f64| {
                let s = x / 255.0;
                if s <= 0.03928 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) }
            };
            0.2126 * f(c.0) + 0.7152 * f(c.1) + 0.0722 * f(c.2)
        }
        let tint = (TINT.0 as f64, TINT.1 as f64, TINT.2 as f64);
        let p = palette();
        for (name, esc) in [("txt", &p.txt), ("dim", &p.dim), ("head", &p.head), ("accent", &p.accent), ("good", &p.good), ("bad", &p.bad), ("warn", &p.warn), ("hint", &p.hint)] {
            let nums: Vec<f64> = esc.trim_start_matches("\x1b[38;2;").trim_end_matches('m').split(';').map(|n| n.parse().unwrap()).collect();
            let c = (nums[0], nums[1], nums[2]);
            for bg in [tint, (0.0, 0.0, 0.0), (16.0, 20.0, 26.0)] {
                let (a, b) = (lum(c), lum(bg));
                let r = (a.max(b) + 0.05) / (a.min(b) + 0.05);
                assert!(r >= 4.5, "{} on {:?} measures {:.2}", name, bg, r);
            }
        }
    }

    #[test]
    fn the_details_show_what_they_fetched() {
        let batch = parse_batch(r#"{"reports":[
          {"dimensionHeaders":[{"name":"date"},{"name":"dateRange"}],"metricHeaders":[{"name":"screenPageViews"},{"name":"totalUsers"}],"rows":[
            {"dimensionValues":[{"value":"20261001"},{"value":"cur"}],"metricValues":[{"value":"100"},{"value":"10"}]},
            {"dimensionValues":[{"value":"20260901"},{"value":"prev"}],"metricValues":[{"value":"50"},{"value":"5"}]}]},
          {"dimensionHeaders":[{"name":"sessionSourceMedium"}],"metricHeaders":[{"name":"screenPageViews"}],"rows":[{"dimensionValues":[{"value":"google / organic"}],"metricValues":[{"value":"70"}]}]},
          {"dimensionHeaders":[{"name":"country"}],"metricHeaders":[{"name":"screenPageViews"}]},
          {"dimensionHeaders":[{"name":"deviceCategory"}],"metricHeaders":[{"name":"screenPageViews"}],"rows":[{"dimensionValues":[{"value":"desktop"}],"metricValues":[{"value":"60"}]}]},
          {"dimensionHeaders":[{"name":"eventName"}],"metricHeaders":[{"name":"keyEvents"}]}
        ]}"#).unwrap();
        let all = text(&page_detail("/pricing", Some(&Ok(batch)), 100));
        assert!(all.contains("views 28d 100 vs prior 50 (+100%)"), "{}", all);
        assert!(all.contains("google / organic"));
        assert!(all.contains("not matched to a live count"));
        let prop = full_prop();
        let all = text(&property_detail(&prop, &view(), 100));
        for want in ["timezone UTC", "currency USD", "tokensPerHour", "potentiallyThresholdedRequestsPerHour", "123.samples", "2 samples over 1.0 weeks"] {
            assert!(all.contains(want), "missing {:?}\n{}", want, all);
        }
    }
}
