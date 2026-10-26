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

//! What a region is searching for right now, from Google's own feed.
//!
//! One source: the Trending Now RSS feed at
//! `trends.google.com/trending/rss?geo=<GEO>`, fetched with curl. No
//! account, no key, no scraping. Everything Google says is shown as it
//! says it - the traffic bucket verbatim, the order unchanged. Everything
//! the widget adds (SEEN, `new since`, LEFT THE FEED) is its own
//! observation across fetches and is labelled that way.
//!
//!     google-trends [-n SECONDS] [GEO ...]

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use opscope_core as tc;

mod parse;
#[path = "../refresh_wake.rs"]
mod refresh_wake;
mod state;

use parse::{Answer, NewsItem, Trend};
use state::{Left, Memory};

const SETTINGS: tc::SettingsSpec = tc::SettingsSpec {
    widget: "google-trends",
    section: "google_trends",
    legacy_section: None,
    schema: include_str!("settings.json"),
    catalogues: &[],
};

const FEED: &str = "https://trends.google.com/trending/rss?geo=";
const EXPLORE: &str = "https://trends.google.com/trends/explore";
const DEFAULT_REFRESH: f64 = 300.0;
const MIN_REFRESH: f64 = 60.0;
/// The separator between facts on one line. Kept apart from any string
/// that names a key, so a status line never reads as a footer.
const SEP: &str = " · ";

struct Palette {
    title: String,
    head: String,
    text: String,
    dim: String,
    traffic: String,
    fresh: String,
    ok: String,
    warn: String,
    bad: String,
    accent: String,
    /// The selected-row tint. Every colour above measures at least 4.5:1
    /// against it - `palette_clears_aa_on_the_tint` holds that.
    tint: String,
}

fn palette() -> Palette {
    Palette {
        title: tc::rgb(150, 210, 255),
        head: tc::rgb(120, 220, 255),
        text: tc::rgb(225, 235, 245),
        dim: tc::rgb(160, 175, 190),
        traffic: tc::rgb(255, 190, 110),
        fresh: tc::rgb(255, 220, 120),
        ok: tc::rgb(120, 230, 160),
        warn: tc::rgb(255, 200, 90),
        bad: tc::rgb(255, 140, 130),
        accent: tc::rgb(150, 210, 255),
        tint: tc::bg(38, 56, 76),
    }
}

/// Why the last fetch of a region did not produce a feed.
#[derive(Clone, Debug, PartialEq)]
enum Failure {
    /// HTTP 400: Google does not know the region code.
    Rejected(String),
    /// Network, 429, 5xx: the last good rows stay up behind it.
    Failed(String),
    /// A 2xx that would not parse, with the parser's reason.
    Unreadable(String),
}

/// One region, as the pane draws it.
#[derive(Clone, Debug, Default)]
struct Region {
    geo: String,
    items: Vec<Trend>,
    /// Per item: consecutive fetches this widget has seen it in.
    seen: Vec<u32>,
    new_count: usize,
    /// The fetch this one was compared with; `None` is a first check.
    since: Option<f64>,
    /// The previous fetch, when it was too old to compare with.
    stale: Option<f64>,
    left: Vec<Left>,
    /// The last fetch that returned a feed.
    fetched: f64,
    /// When the next fetch is due.
    next_at: f64,
    failure: Option<Failure>,
    /// Something about the remembered history worth saying.
    memory_note: Option<String>,
}

#[derive(Default)]
struct Shared {
    regions: Vec<Region>,
    /// Why the poller stopped, if it has.
    err: String,
}

/// How many refresh intervals may pass between two fetches of a region
/// before the earlier one is too old to compare with. Two missed fetches
/// are a blip; more is a widget that was not watching - closed, asleep or
/// offline - and cannot say what ran on or when anything left.
const COMPARE_WITHIN: f64 = 3.0;

/// Fold one fetch's answer into a region and its memory.
fn apply(region: &mut Region, answer: Answer, memory: &mut Memory, now: f64, refresh: f64) {
    region.next_at = now + refresh;
    match answer {
        Answer::Items(items) => {
            let seen = memory.observe(&items, now, refresh * COMPARE_WITHIN);
            region.items = items;
            region.seen = seen.seen;
            region.new_count = seen.new_count;
            region.since = seen.since;
            region.stale = seen.stale;
            region.left = memory.left.clone();
            region.fetched = now;
            region.failure = None;
        }
        Answer::Rejected(said) => {
            // Not a region Google knows: there were never rows to keep.
            region.items.clear();
            region.seen.clear();
            region.left.clear();
            region.failure = Some(Failure::Rejected(said));
        }
        Answer::Failed(said) => region.failure = Some(Failure::Failed(said)),
        Answer::Unreadable(said) => region.failure = Some(Failure::Unreadable(said)),
    }
}

/// `$XDG_STATE_HOME/opscope/google-trends`, as `ports` and `clocks` place
/// their own state.
fn state_dir() -> std::path::PathBuf {
    let base = std::env::var("XDG_STATE_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("{}/.local/state", std::env::var("HOME").unwrap_or_default()));
    std::path::Path::new(&base)
        .join("opscope")
        .join("google-trends")
}

/// A region's memory, and a note when there is something to say about it.
///
/// A missing file is a first check and says nothing beyond that. A file
/// that will not read is said out loud, because starting fresh over it
/// resets every SEEN count and the reader should know why.
fn load_memory(geo: &str) -> (Memory, Option<String>) {
    let path = state_dir().join(format!("{}.json", state::file_stem(geo)));
    match std::fs::read_to_string(&path) {
        Ok(text) => match state::parse_state(&text) {
            Ok(memory) => (memory, None),
            Err(why) => (
                Memory::default(),
                Some(format!(
                    "saved history would not read ({why}), started fresh"
                )),
            ),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Memory::default(), None),
        Err(e) => (
            Memory::default(),
            Some(format!("saved history would not open ({e}), started fresh")),
        ),
    }
}

/// Write a region's memory beside the others, replacing it whole.
fn save_memory(geo: &str, memory: &Memory) -> Result<(), String> {
    let dir = state_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let stem = state::file_stem(geo);
    let tmp = dir.join(format!(".{stem}.json.tmp"));
    std::fs::write(&tmp, memory.to_json()).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, dir.join(format!("{stem}.json"))).map_err(|e| e.to_string())
}

/// The memory note after a save: a failure replaces it, a success ends a
/// failure, and the started-fresh notice goes once there is a fetch to compare with.
fn note_after_save(note: Option<String>, saved: Result<(), String>, compared: bool) -> Option<String> {
    match saved {
        Err(why) => Some(format!("could not save what was seen ({why})")),
        Ok(()) => match note {
            Some(n) if n.starts_with("could not save") => None,
            Some(n) if n.contains("started fresh") && compared => None,
            other => other,
        },
    }
}

/// Percent-encode for a query string.
fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// The Explore page for a term. Only ever copied, never fetched.
fn explore_url(term: &str, geo: &str) -> String {
    format!("{}?q={}&geo={}", EXPLORE, encode(term), encode(geo))
}

/// One fetch of one region's feed.
fn fetch(geo: &str) -> Answer {
    match tc::get_answer(&format!("{}{}", FEED, encode(geo)), &[], 20) {
        Ok((status, body, _)) => parse::parse_answer(&body, status, geo),
        Err(said) => Answer::Failed(said),
    }
}

/// A Unix time as local `HH:MM`, with the weekday when it is not today.
fn clock(secs: f64, now: f64) -> String {
    use chrono::TimeZone;
    let local = |t: f64| chrono::Local.timestamp_opt(t as i64, 0).single();
    match (local(secs), local(now)) {
        (Some(at), Some(today)) if at.date_naive() == today.date_naive() => {
            at.format("%H:%M").to_string()
        }
        (Some(at), _) => at.format("%a %H:%M").to_string(),
        _ => "--".into(),
    }
}

/// "every 5 min" for a refresh in seconds.
fn cadence(refresh: f64) -> String {
    if refresh >= 60.0 && (refresh % 60.0).abs() < 0.5 {
        format!("every {} min", (refresh / 60.0).round() as i64)
    } else {
        format!("every {}s", refresh.round() as i64)
    }
}

/// Break `pieces` onto lines no wider than `width`, joined by `SEP`,
/// never splitting a piece unless it alone is wider than the line.
fn pack(pieces: &[String], width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out: Vec<String> = Vec::new();
    let mut line = String::new();
    for piece in pieces.iter().filter(|p| !p.is_empty()) {
        let extra = if line.is_empty() {
            0
        } else {
            tc::display_width(SEP)
        };
        if !line.is_empty() && tc::display_width(&line) + extra + tc::display_width(piece) > width {
            out.push(std::mem::take(&mut line));
        }
        if tc::display_width(piece) > width {
            if !line.is_empty() {
                out.push(std::mem::take(&mut line));
            }
            let mut wrapped = tc::wrap_words(piece, width);
            let last = wrapped.pop().unwrap_or_default();
            out.extend(wrapped);
            line = last;
            continue;
        }
        if !line.is_empty() {
            line.push_str(SEP);
        }
        line.push_str(piece);
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
}

/// A line of text at an indent, coloured, at most `cw` wide.
///
/// Wrapped rather than clipped: a sentence too long for the pane goes onto
/// the next row instead of losing its end.
fn line(colour: &str, indent: usize, text: &str, cw: usize) -> Vec<String> {
    tc::wrap_words(text, cw.saturating_sub(indent).max(1))
        .into_iter()
        .map(|l| clip(&[(colour, format!("{}{}", " ".repeat(indent), l))], cw))
        .collect()
}

/// `tc::seg`, and under test a failure if anything but padding was cut.
/// Every row on screen goes through here, so the width tests catch a
/// value that was truncated rather than merely a row that was too wide.
fn clip(parts: &[(&str, String)], cw: usize) -> String {
    // `cfg!` rather than the attribute: check.rs reads a widget's source
    // up to its first test attribute, and one here would hide the rest.
    if cfg!(test) {
        let text: String = parts.iter().map(|(_, t)| t.as_str()).collect();
        let used = tc::display_width(text.trim_end());
        assert!(used <= cw, "{used} > {cw}: {text:?} would be cut");
    }
    tc::seg(parts, cw)
}

/// What a trend matches a filter against: its term and its news.
fn matches(needle: &str, title: &str, news: &[NewsItem]) -> bool {
    if needle.is_empty() {
        return true;
    }
    let n = needle.to_lowercase();
    title.to_lowercase().contains(&n)
        || news
            .iter()
            .any(|s| s.source.to_lowercase().contains(&n) || s.title.to_lowercase().contains(&n))
}

/// The distinct sources of a trend's news, in feed order.
fn sources(news: &[NewsItem]) -> Vec<String> {
    let mut seen = HashSet::new();
    news.iter()
        .map(|n| n.source.clone())
        .filter(|s| !s.is_empty() && seen.insert(s.clone()))
        .collect()
}

/// When Google says a trend started: local time and how long ago.
fn started(t: &Trend, now: f64) -> String {
    match t.started {
        Some(at) => format!("{} {}", clock(at as f64, now), tc::age(now - at as f64)),
        None => "--".into(),
    }
}

/// The SEEN cell: `new` the first fetch a trend appears in after a
/// previous one, otherwise how many consecutive fetches.
fn seen_cell(polls: u32, first_check: bool) -> String {
    if polls <= 1 && !first_check {
        "new".into()
    } else {
        format!("{polls}×")
    }
}

/// Section indices.
const TRENDING: usize = 0;
const LEFT: usize = 1;
const REGIONS: usize = 2;

/// Everything the list screen needs from the keyboard side.
#[derive(Clone, Debug, Default)]
struct View {
    region: usize,
    /// The section the cursor is in this frame.
    focus: usize,
    /// The section the reader last chose. Focus falls back to another
    /// section while this one is empty - before the first fetch, or under a
    /// filter - and returns here as soon as it has rows again.
    wanted: usize,
    at: [usize; 3],
    needle: String,
    typing: bool,
    note: String,
}

/// A built body: its rows, which item each row belongs to, and where the
/// cursor is.
#[derive(Default)]
struct Body {
    rows: Vec<String>,
    /// `(body row, slot)` for every row an item occupies.
    placed: Vec<(usize, usize)>,
    /// `(section, index)` per slot.
    slots: Vec<(usize, usize)>,
    cursor: Option<usize>,
    /// How many rows each section has to select, after the filter.
    lens: [usize; 3],
    /// Visible indices per section, after the filter.
    visible: [Vec<usize>; 3],
}

/// Widths of the trending table's columns, or `0` for a dropped one.
///
/// `stacked` is the layout below every table: too narrow for SEARCH and
/// TRAFFIC side by side, so each trend is its term with its bucket under
/// it, in one column `search` wide.
#[derive(Debug, PartialEq)]
struct Columns {
    search: usize,
    traffic: usize,
    started: usize,
    seen: usize,
    news: usize,
    stacked: bool,
}

const LEAD: usize = 2;

/// Choose the trending table's columns for a content width.
///
/// NEWS goes first as the pane narrows, then SEEN, then STARTED; SEARCH
/// and TRAFFIC always stay, because a term without its bucket is not the
/// reading. No value is cut: SEARCH and NEWS wrap within their column.
fn columns(
    cw: usize,
    traffic: usize,
    started: usize,
    seen: usize,
    search_want: usize,
    news_want: usize,
) -> Columns {
    let gap = 1usize;
    // A column is never narrower than its heading, or the heading is cut.
    let search_want = search_want.max("SEARCH".len());
    let news_want = news_want.max("NEWS".len());
    let search_min = search_want.min(12);
    let traffic = traffic.max("TRAFFIC".len());
    let started = started.max("STARTED".len());
    let seen = seen.max("SEEN".len());
    let room = cw.saturating_sub(LEAD);
    // All five.
    let fixed = traffic + started + seen + gap * 4;
    let news_min = 12usize;
    if room >= fixed + search_min + news_min && news_want > 0 {
        let fit = tc::fit_columns(
            room - fixed,
            &[(search_min, search_want), (news_min, news_want)],
        );
        return Columns {
            search: fit[0],
            traffic,
            started,
            seen,
            news: fit[1],
            stacked: false,
        };
    }
    for (with_seen, with_started) in [(true, true), (false, true), (false, false)] {
        let fixed = traffic
            + gap
            + if with_started { started + gap } else { 0 }
            + if with_seen { seen + gap } else { 0 };
        // The last table needs only SEARCH's heading, since its terms wrap.
        let least = if with_started { search_min } else { "SEARCH".len() };
        if room >= fixed + least {
            return Columns {
                search: room - fixed,
                traffic,
                started: if with_started { started } else { 0 },
                seen: if with_seen { seen } else { 0 },
                news: 0,
                stacked: false,
            };
        }
    }
    Columns {
        search: room.max(1),
        traffic: 0,
        started: 0,
        seen: 0,
        news: 0,
        stacked: true,
    }
}

/// The list screen's body, below the pinned title.
fn list_body(shared: &Shared, view: &View, refresh: f64, cw: usize, now: f64, p: &Palette) -> Body {
    let mut b = Body::default();
    if !shared.err.is_empty() {
        b.rows.extend(tc::error_rows(&p.bad, &shared.err, cw + 1));
    }
    let Some(region) = shared.regions.get(view.region) else {
        b.rows.extend(line(&p.dim, 1, "no regions configured", cw));
        return b;
    };
    let several = shared.regions.len() > 1;
    let first_check = region.since.is_none();

    // Status: when, where from, and which region of how many.
    let mut status = vec![if region.fetched > 0.0 {
        format!("fetched {} ago", tc::age(now - region.fetched))
    } else if region.failure.is_some() {
        "no successful fetch yet".to_string()
    } else {
        "fetching…".to_string()
    }];
    status.push(format!("Google's Trending Now feed, {}", cadence(refresh)));
    if several {
        status.push(region_index(view.region, shared.regions.len()));
    }
    for l in pack(&status, cw.saturating_sub(1)) {
        b.rows.extend(line(&p.dim, 1, &l, cw));
    }
    if let Some(note) = &region.memory_note {
        for l in tc::wrap_words(note, cw.saturating_sub(1)) {
            b.rows.extend(line(&p.warn, 1, &l, cw));
        }
    }
    if !view.note.is_empty() {
        for l in tc::wrap_words(&view.note, cw.saturating_sub(1)) {
            b.rows.extend(line(&p.ok, 1, &l, cw));
        }
    }
    let filters: Vec<String> = if view.needle.is_empty() {
        Vec::new()
    } else {
        vec![format!("/{}", view.needle)]
    };
    if view.typing {
        b.rows
            .extend(line(&p.accent, 1, &format!("/{}▏", view.needle), cw));
    }
    b.rows.push(String::new());

    // TRENDING NOW
    b.visible[TRENDING] = (0..region.items.len())
        .filter(|&i| matches(&view.needle, &region.items[i].title, &region.items[i].news))
        .collect();
    b.visible[LEFT] = (0..region.left.len())
        .filter(|&i| now - region.left[i].left_at <= state::LEFT_KEPT)
        .filter(|&i| {
            let l = &region.left[i];
            matches(&view.needle, &l.title, l.news.as_deref().unwrap_or_default())
        })
        .collect();
    if several {
        b.visible[REGIONS] = (0..shared.regions.len()).collect();
    }
    for s in 0..3 {
        b.lens[s] = b.visible[s].len();
    }

    b.rows.extend(line(
        &p.head,
        1,
        &format!("TRENDING NOW{SEP}{}", region.geo),
        cw,
    ));
    let mut about = vec!["Google's \"approx traffic\" is a bucket, not a count".to_string()];
    match tc::filter_row(b.lens[TRENDING], region.items.len(), &filters) {
        Some(f) => about.push(f),
        None if region.items.is_empty() => {}
        None => about.push(format!("{} of {}", b.lens[TRENDING], region.items.len())),
    }
    if region.fetched > 0.0 {
        match region.since {
            Some(at) => about.push(format!("{} new since {}", region.new_count, clock(at, now))),
            None => match region.stale {
                Some(at) => about.push(format!(
                    "previous fetch {} ago, too long to compare; starting over",
                    tc::age(now - at)
                )),
                None => about.push("first check, nothing to compare yet".to_string()),
            },
        }
    }
    for l in pack(&about, cw.saturating_sub(1)) {
        b.rows.extend(line(&p.dim, 1, &l, cw));
    }

    let retry = if region.next_at > now {
        format!("retrying in {}", tc::age(region.next_at - now))
    } else {
        "retrying now".to_string()
    };
    match &region.failure {
        Some(Failure::Rejected(said)) => {
            let msg = tc::missing_config(&format!("{said} (HTTP 400) - fix google_trends.geos"));
            b.rows.extend(tc::error_rows(&p.bad, &msg, cw + 1));
        }
        Some(Failure::Failed(said)) | Some(Failure::Unreadable(said)) => {
            let what = match &region.failure {
                Some(Failure::Unreadable(_)) => format!("Google's feed would not parse: {said}"),
                _ => format!("fetch failed: {said}"),
            };
            let kept = if region.fetched > 0.0 {
                format!(
                    "{what}{SEP}showing the fetch from {} ago{SEP}{retry}",
                    tc::age(now - region.fetched)
                )
            } else {
                format!("{what}{SEP}{retry}")
            };
            b.rows.extend(tc::error_rows(&p.warn, &kept, cw + 1));
        }
        None => {}
    }

    let rejected = matches!(region.failure, Some(Failure::Rejected(_)));
    if region.fetched <= 0.0 && region.failure.is_none() {
        b.rows.extend(line(
            &p.dim,
            1,
            &format!("fetching Google's feed for {}…", region.geo),
            cw,
        ));
    } else if region.fetched > 0.0 && region.items.is_empty() && !rejected {
        for l in tc::wrap_words(
            &format!("Google's feed for {} has no items right now", region.geo),
            cw.saturating_sub(1),
        ) {
            b.rows.extend(line(&p.dim, 1, &l, cw));
        }
    } else if !region.items.is_empty() && b.lens[TRENDING] == 0 {
        if let Some(said) = tc::filtered_to_nothing(region.items.len(), &filters) {
            for l in tc::wrap_words(&said, cw.saturating_sub(1)) {
                b.rows.extend(line(&p.dim, 1, &l, cw));
            }
        }
    } else if !region.items.is_empty() {
        trending_rows(&mut b, region, view, first_check, cw, now, p);
    }

    // LEFT THE FEED
    b.rows.push(String::new());
    b.rows.extend(line(&p.head, 1, "LEFT THE FEED", cw));
    let kept =
        format!("this widget's own observation{SEP}gone since the fetch before, kept an hour");
    for l in tc::wrap_words(&kept, cw.saturating_sub(1)) {
        b.rows.extend(line(&p.dim, 1, &l, cw));
    }
    let shown_left: Vec<usize> = (0..region.left.len())
        .filter(|&i| now - region.left[i].left_at <= state::LEFT_KEPT)
        .collect();
    if shown_left.is_empty() {
        let none = if first_check {
            "nothing yet: it takes two fetches to see one leave"
        } else {
            "nothing has left the feed in the last hour"
        };
        for l in tc::wrap_words(none, cw.saturating_sub(1)) {
            b.rows.extend(line(&p.dim, 1, &l, cw));
        }
    } else if b.lens[LEFT] == 0 {
        if let Some(said) = tc::filtered_to_nothing(shown_left.len(), &filters) {
            for l in tc::wrap_words(&said, cw.saturating_sub(1)) {
                b.rows.extend(line(&p.dim, 1, &l, cw));
            }
        }
    } else {
        left_rows(&mut b, region, view, cw, now, p);
    }

    // REGIONS
    if several {
        b.rows.push(String::new());
        b.rows.extend(line(&p.head, 1, "REGIONS", cw));
        region_rows(&mut b, shared, view, cw, now, p);
    }
    b
}

/// `←→ 1 of 3`, on a line of its own so the arrows never share a string
/// with a separator.
fn region_index(at: usize, of: usize) -> String {
    format!("←→ {} of {}", at + 1, of)
}

/// Push one selectable item's rows, recording where they landed.
fn push_item(b: &mut Body, section: usize, index: usize, rows: Vec<String>, selected: bool) {
    let slot = b.slots.len();
    b.slots.push((section, index));
    let from = b.rows.len();
    if selected {
        b.cursor = Some(from);
    }
    b.rows.extend(rows);
    b.placed.extend((from..b.rows.len()).map(|row| (row, slot)));
}

/// Colour on the tint when selected, plain otherwise.
fn on(p: &Palette, colour: &str, selected: bool) -> String {
    if selected {
        format!("{}{}", p.tint, colour)
    } else {
        colour.to_string()
    }
}

/// The title of item `i` of section `s` in a region, for the two trend
/// sections.
fn title_of(region: &Region, s: usize, i: usize) -> Option<&str> {
    match s {
        TRENDING => region.items.get(i).map(|t| t.title.as_str()),
        LEFT => region.left.get(i).map(|l| l.title.as_str()),
        _ => None,
    }
}

/// What the cursor is on in each trend section: the region, and the title
/// per section. A refresh reorders the feed, and the cursor follows this,
/// not its row number.
type Picked = (usize, [Option<String>; 2]);

fn picked(shared: &Shared, view: &View, body: &Body) -> Picked {
    let titles = [TRENDING, LEFT].map(|s| {
        let region = shared.regions.get(view.region)?;
        let &i = body.visible[s].get(view.at[s])?;
        title_of(region, s, i).map(str::to_string)
    });
    (view.region, titles)
}

/// Put the cursor back on the titles it was on, wherever the new fetch
/// moved them. A title that has gone keeps the row number instead, the
/// only position left to keep.
fn follow_titles(view: &mut View, shared: &Shared, body: &Body, was: &Picked) {
    if was.0 != view.region {
        return;
    }
    let Some(region) = shared.regions.get(view.region) else {
        return;
    };
    for (k, s) in [TRENDING, LEFT].into_iter().enumerate() {
        let Some(title) = was.1[k].as_deref() else {
            continue;
        };
        if let Some(pos) = body.visible[s]
            .iter()
            .position(|&i| title_of(region, s, i) == Some(title))
        {
            view.at[s] = pos;
        }
    }
}

/// Lay cells out side by side; each cell is a column of wrapped lines.
fn table_row(
    cells: &[(String, usize, Vec<String>)],
    p: &Palette,
    selected: bool,
    cw: usize,
) -> Vec<String> {
    let height = cells
        .iter()
        .map(|(_, _, l)| l.len())
        .max()
        .unwrap_or(1)
        .max(1);
    let marker = if selected { "▸ " } else { "  " };
    (0..height)
        .map(|r| {
            let mut parts: Vec<(String, String)> = vec![(
                on(p, &p.accent, selected),
                if r == 0 {
                    marker.to_string()
                } else {
                    "  ".to_string()
                },
            )];
            for (i, (colour, width, lines)) in cells.iter().enumerate() {
                let text = lines.get(r).cloned().unwrap_or_default();
                if cfg!(test) {
                    assert!(
                        tc::display_width(&text) <= *width,
                        "{text:?} cut to {width}"
                    );
                }
                let gap = if i + 1 < cells.len() { " " } else { "" };
                parts.push((
                    on(p, colour, selected),
                    format!("{}{}", tc::pad(&text, *width), gap),
                ));
            }
            let used: usize = parts.iter().map(|(_, t)| tc::display_width(t)).sum();
            if selected && used < cw {
                parts.push((on(p, &p.text, true), " ".repeat(cw - used)));
            }
            let borrowed: Vec<(&str, String)> =
                parts.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
            format!("{}{}", clip(&borrowed, cw), tc::RST)
        })
        .collect()
}

fn trending_rows(
    b: &mut Body,
    region: &Region,
    view: &View,
    first_check: bool,
    cw: usize,
    now: f64,
    p: &Palette,
) {
    let vis = b.visible[TRENDING].clone();
    let w_of = |f: &dyn Fn(usize) -> String| {
        vis.iter()
            .map(|&i| tc::display_width(&f(i)))
            .max()
            .unwrap_or(0)
    };
    let traffic_w = w_of(&|i| region.items[i].traffic.clone());
    let started_w = w_of(&|i| started(&region.items[i], now));
    let seen_w = w_of(&|i| seen_cell(region.seen.get(i).copied().unwrap_or(1), first_check));
    let search_want = w_of(&|i| region.items[i].title.clone());
    let news_want = w_of(&|i| sources(&region.items[i].news).join(SEP));
    let c = columns(cw, traffic_w, started_w, seen_w, search_want, news_want);
    if c.stacked {
        let w = c.search;
        let head: Vec<(String, String)> = tc::wrap_words("SEARCH / TRAFFIC", w)
            .into_iter()
            .map(|l| (p.dim.clone(), l))
            .collect();
        b.rows.extend(stacked(head, p, false, cw));
        let selected = (view.focus == TRENDING).then(|| view.at[TRENDING]);
        for (k, &i) in vis.iter().enumerate() {
            let t = &region.items[i];
            let mut lines: Vec<(String, String)> = tc::wrap_words(&t.title, w)
                .into_iter()
                .map(|l| (p.text.clone(), l))
                .collect();
            lines.extend(
                tc::wrap_words(&t.traffic, w)
                    .into_iter()
                    .map(|l| (p.traffic.clone(), l)),
            );
            let sel = selected == Some(k);
            let rows = stacked(lines, p, sel, cw);
            push_item(b, TRENDING, k, rows, sel);
        }
        return;
    }

    let mut head: Vec<(String, usize, Vec<String>)> =
        vec![(p.dim.clone(), c.search, vec!["SEARCH".into()])];
    head.push((p.dim.clone(), c.traffic, vec!["TRAFFIC".into()]));
    if c.started > 0 {
        head.push((p.dim.clone(), c.started, vec!["STARTED".into()]));
    }
    if c.seen > 0 {
        head.push((p.dim.clone(), c.seen, vec!["SEEN".into()]));
    }
    if c.news > 0 {
        head.push((p.dim.clone(), c.news, vec!["NEWS".into()]));
    }
    b.rows.extend(table_row(&head, p, false, cw));

    let selected = (view.focus == TRENDING).then(|| view.at[TRENDING]);
    for (k, &i) in vis.iter().enumerate() {
        let t = &region.items[i];
        let polls = region.seen.get(i).copied().unwrap_or(1);
        let mut cells: Vec<(String, usize, Vec<String>)> =
            vec![(p.text.clone(), c.search, tc::wrap_words(&t.title, c.search))];
        cells.push((p.traffic.clone(), c.traffic, vec![t.traffic.clone()]));
        if c.started > 0 {
            cells.push((p.dim.clone(), c.started, vec![started(t, now)]));
        }
        if c.seen > 0 {
            let colour = if polls <= 1 && !first_check {
                &p.fresh
            } else {
                &p.dim
            };
            cells.push((colour.clone(), c.seen, vec![seen_cell(polls, first_check)]));
        }
        if c.news > 0 {
            let src = sources(&t.news);
            let lines = if src.is_empty() {
                vec!["--".to_string()]
            } else {
                pack(&src, c.news)
            };
            cells.push((p.dim.clone(), c.news, lines));
        }
        let sel = selected == Some(k);
        let rows = table_row(&cells, p, sel, cw);
        push_item(b, TRENDING, k, rows, sel);
    }
}

/// One item drawn as stacked lines in a single column, the marker on its
/// first line only.
fn stacked(lines: Vec<(String, String)>, p: &Palette, selected: bool, cw: usize) -> Vec<String> {
    let width = cw.saturating_sub(LEAD).max(1);
    let colours: Vec<String> = lines.iter().map(|(c, _)| c.clone()).collect();
    let texts: Vec<String> = lines.into_iter().map(|(_, t)| t).collect();
    let mut rows = table_row(&[(String::new(), width, texts)], p, selected, cw);
    // table_row draws one colour per cell; recolour each line's text.
    for (row, colour) in rows.iter_mut().zip(colours) {
        let plain = tc::source_text(row);
        let (marker, rest) =
            plain.split_at(plain.char_indices().nth(2).map_or(plain.len(), |(i, _)| i));
        let parts = vec![
            (on(p, &p.accent, selected), marker.to_string()),
            (on(p, &colour, selected), rest.to_string()),
        ];
        let borrowed: Vec<(&str, String)> =
            parts.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
        *row = format!("{}{}", clip(&borrowed, cw), tc::RST);
    }
    rows
}

/// A name and its facts: side by side when there is room, the facts
/// wrapped under the name when there is not.
fn named_rows(
    name: &str,
    name_want: usize,
    name_colour: &str,
    facts: &[String],
    p: &Palette,
    selected: bool,
    cw: usize,
) -> Vec<String> {
    let room = cw.saturating_sub(LEAD);
    let name_w = name_want.min(room / 2).max(1);
    let fact_w = room.saturating_sub(name_w + 1);
    if fact_w >= 18 {
        let cells = vec![
            (
                name_colour.to_string(),
                name_w,
                tc::wrap_words(name, name_w),
            ),
            (p.dim.clone(), fact_w, pack(facts, fact_w)),
        ];
        table_row(&cells, p, selected, cw)
    } else {
        let mut lines: Vec<(String, String)> = tc::wrap_words(name, room.max(1))
            .into_iter()
            .map(|l| (name_colour.to_string(), l))
            .collect();
        for l in pack(facts, room.saturating_sub(2).max(1)) {
            lines.push((p.dim.clone(), format!("  {l}")));
        }
        stacked(lines, p, selected, cw)
    }
}

fn left_rows(b: &mut Body, region: &Region, view: &View, cw: usize, now: f64, p: &Palette) {
    let selected = (view.focus == LEFT).then(|| view.at[LEFT]);
    let name_want = b.visible[LEFT]
        .iter()
        .map(|&i| tc::display_width(&region.left[i].title))
        .max()
        .unwrap_or(1);
    for (k, &i) in b.visible[LEFT].clone().iter().enumerate() {
        let l = &region.left[i];
        let mut facts = Vec::new();
        if !l.traffic.is_empty() {
            facts.push(format!("was {}", l.traffic));
        }
        facts.push(if l.polls <= 1 {
            "in 1 fetch".to_string()
        } else {
            format!(
                "in {} fetches over {}",
                l.polls,
                tc::age(l.last_seen - l.first_seen)
            )
        });
        facts.push(format!("gone {} ago", tc::age(now - l.left_at)));
        let sel = selected == Some(k);
        let rows = named_rows(&l.title, name_want, &p.text, &facts, p, sel, cw);
        push_item(b, LEFT, k, rows, sel);
    }
}

/// The trend that started most recently, Google's order breaking a tie.
fn newest(region: &Region) -> Option<&Trend> {
    let mut best: Option<&Trend> = None;
    for t in &region.items {
        match (best, t.started) {
            (None, _) => best = Some(t),
            (Some(b), Some(s)) if b.started.is_none_or(|bs| s > bs) => best = Some(t),
            _ => {}
        }
    }
    best
}

fn region_rows(b: &mut Body, shared: &Shared, view: &View, cw: usize, now: f64, p: &Palette) {
    let selected = (view.focus == REGIONS).then(|| view.at[REGIONS]);
    let name_want = shared
        .regions
        .iter()
        .map(|r| tc::display_width(&r.geo) + 2)
        .max()
        .unwrap_or(1);
    for (k, r) in shared.regions.iter().enumerate() {
        let mut facts = Vec::new();
        let colour = match &r.failure {
            Some(Failure::Rejected(said)) => {
                facts.push(format!("{said} (HTTP 400)"));
                &p.bad
            }
            other => {
                match newest(r) {
                    Some(t) if t.traffic.is_empty() => facts.push(format!("newest: {}", t.title)),
                    Some(t) => facts.push(format!("newest: {} {}", t.title, t.traffic)),
                    None if r.fetched > 0.0 => facts.push("no items right now".into()),
                    None => {}
                }
                if r.fetched > 0.0 {
                    facts.push(format!("fetched {} ago", tc::age(now - r.fetched)));
                } else if other.is_none() {
                    facts.push("fetching…".into());
                }
                match other {
                    Some(Failure::Failed(said)) => {
                        facts.push(format!("last fetch failed: {said}"));
                        &p.warn
                    }
                    Some(Failure::Unreadable(_)) => {
                        facts.push("last fetch would not parse".into());
                        &p.warn
                    }
                    _ => &p.text,
                }
            }
        };
        let name = if k == view.region {
            format!("{} ◂", r.geo)
        } else {
            r.geo.clone()
        };
        let sel = selected == Some(k);
        let rows = named_rows(&name, name_want, colour, &facts, p, sel, cw);
        push_item(b, REGIONS, k, rows, sel);
    }
}

/// What the detail screen is showing.
#[derive(Clone, Debug, Default)]
struct Detail {
    geo: String,
    title: String,
    traffic: String,
    started: Option<i64>,
    started_raw: String,
    /// Consecutive fetches, and when the run began; or, for one that has
    /// gone, when it went.
    seen: String,
    news: Vec<NewsItem>,
    /// A trend that has left the feed: its start time is not kept.
    gone: bool,
    /// Whether `news` is what Google attached, or the stories were not
    /// kept (a departure from before a restart). Not kept is not none.
    news_kept: bool,
    at: usize,
}

fn detail_body(d: &Detail, cw: usize, now: f64, p: &Palette) -> Body {
    let mut b = Body::default();
    let room = cw.saturating_sub(1).max(1);
    for l in tc::wrap_words(&d.title, room) {
        b.rows.extend(line(&p.text, 1, &l, cw));
    }
    let mut facts = Vec::new();
    if d.traffic.is_empty() {
        facts.push("no traffic bucket in the feed".to_string());
    } else {
        facts.push(format!("approx traffic {}", d.traffic));
        facts.push("a bucket, not a count".to_string());
    }
    facts.push(match (d.started, d.started_raw.is_empty()) {
        (Some(at), _) => format!(
            "started {} ({} ago)",
            clock(at as f64, now),
            tc::age(now - at as f64)
        ),
        (None, false) => format!("started \"{}\", which would not parse", d.started_raw),
        (None, true) if d.gone => "start time not kept once a trend leaves".to_string(),
        (None, true) => "no start time in the feed".to_string(),
    });
    facts.push(d.seen.clone());
    facts.push(format!("geo {}", d.geo));
    for l in pack(&facts, room) {
        b.rows.extend(line(&p.dim, 1, &l, cw));
    }
    b.rows.push(String::new());
    if !d.news_kept {
        b.rows.extend(line(&p.head, 1, &format!("NEWS{SEP}not kept"), cw));
        let why = "stories are kept only while the widget runs, and it has \
                   restarted since this trend left";
        for l in tc::wrap_words(why, room) {
            b.rows.extend(line(&p.dim, 1, &l, cw));
        }
        return b;
    }
    b.rows.extend(line(
        &p.head,
        1,
        &format!(
            "NEWS{SEP}{} stor{} Google attaches",
            d.news.len(),
            if d.news.len() == 1 { "y" } else { "ies" }
        ),
        cw,
    ));
    if d.news.is_empty() {
        b.rows.extend(line(
            &p.dim,
            1,
            "Google lists no stories for this trend",
            cw,
        ));
    }
    b.lens[0] = d.news.len();
    for (k, n) in d.news.iter().enumerate() {
        let sel = k == d.at;
        let inner = cw.saturating_sub(LEAD).max(1);
        let mut cells: Vec<(String, String)> = Vec::new();
        let headline = if n.title.is_empty() {
            "(no headline)"
        } else {
            n.title.as_str()
        };
        for l in tc::wrap_words(headline, inner) {
            cells.push((p.text.clone(), l));
        }
        if !n.source.is_empty() {
            for l in tc::wrap_words(&n.source, inner) {
                cells.push((p.traffic.clone(), l));
            }
        }
        // A URL has no spaces to wrap at, so wrap_words breaks it hard -
        // every character of it is still on screen.
        for l in tc::wrap_words(&n.url, inner) {
            cells.push((p.accent.clone(), l));
        }
        let rows = stacked(cells, p, sel, cw);
        push_item(&mut b, 0, k, rows, sel);
        b.rows.push(String::new());
    }
    b
}

/// The detail for the selected row of the list, if it opens one.
fn detail_for(shared: &Shared, view: &View, body: &Body, now: f64) -> Option<Detail> {
    let region = shared.regions.get(view.region)?;
    let index = *body.visible[view.focus].get(view.at[view.focus])?;
    match view.focus {
        TRENDING => {
            let t = region.items.get(index)?;
            let polls = region.seen.get(index).copied().unwrap_or(1);
            let seen = match region.since {
                None => "first check: seen by this widget once so far".to_string(),
                Some(_) if polls <= 1 => "new in this widget's latest fetch".to_string(),
                Some(_) => format!("seen by this widget in {polls} consecutive fetches"),
            };
            Some(Detail {
                geo: region.geo.clone(),
                title: t.title.clone(),
                traffic: t.traffic.clone(),
                started: t.started,
                started_raw: t.started_raw.clone(),
                seen,
                news: t.news.clone(),
                gone: false,
                news_kept: true,
                at: 0,
            })
        }
        LEFT => {
            let l = region.left.get(index)?;
            Some(Detail {
                geo: region.geo.clone(),
                title: l.title.clone(),
                traffic: l.traffic.clone(),
                started: None,
                started_raw: String::new(),
                seen: format!(
                    "left the feed {} ago, after {} fetch{} (this widget's count)",
                    tc::age(now - l.left_at),
                    l.polls,
                    if l.polls == 1 { "" } else { "es" }
                ),
                news: l.news.clone().unwrap_or_default(),
                gone: true,
                news_kept: l.news.is_some(),
                at: 0,
            })
        }
        _ => None,
    }
}

fn list_hints(p: &Palette, several: bool, view: &View) -> Vec<Vec<(&'static str, String)>> {
    let _ = p;
    if view.typing {
        return vec![
            vec![("", "type to filter".to_string())],
            vec![("", "↵ keep".to_string())],
            vec![("", "esc clear".to_string())],
        ];
    }
    let mut hints = vec![
        vec![("", "↑↓ select".to_string())],
        vec![("", "tab section".to_string())],
    ];
    if several {
        hints.push(vec![("", "←→ region".to_string())]);
    }
    hints.push(vec![("", "[↵] news".to_string())]);
    hints.push(vec![("", "[/] filter".to_string())]);
    if !view.needle.is_empty() {
        hints.push(vec![("", "esc clear filter".to_string())]);
    }
    hints.push(vec![("", "[c] copy link".to_string())]);
    hints.push(vec![("", "[r] refresh".to_string())]);
    hints.push(vec![("", "[,] settings".to_string())]);
    hints.push(vec![("", "[q]uit".to_string())]);
    hints
}

fn detail_hints() -> Vec<Vec<(&'static str, String)>> {
    vec![
        vec![("", "↑↓ select".to_string())],
        vec![("", "[c] copy URL".to_string())],
        vec![("", "esc back".to_string())],
        vec![("", "[q]uit".to_string())],
    ]
}

/// Footer lines, coloured, and the placements for clicks.
fn footer(
    hints: &[Vec<(&'static str, String)>],
    cw: usize,
    p: &Palette,
) -> (tc::Footer, Vec<String>) {
    let coloured: Vec<Vec<(&str, String)>> = hints
        .iter()
        .map(|h| h.iter().map(|(_, t)| (p.dim.as_str(), t.clone())).collect())
        .collect();
    let packed = tc::pack_hints_placed(&coloured, cw.saturating_sub(1), "  ");
    let lines = packed
        .lines
        .iter()
        .map(|l| format!(" {}{}", l, tc::RST))
        .collect();
    (packed, lines)
}

/// The rows of `body` that a window of `room` starting at `scroll` shows.
fn window(body: &[String], scroll: usize, room: usize) -> (Vec<String>, usize) {
    let scroll = scroll.min(body.len().saturating_sub(room));
    let mut shown: Vec<String> = body.iter().skip(scroll).take(room).cloned().collect();
    while shown.len() < room {
        shown.push(String::new());
    }
    (shown, scroll)
}

fn main() {
    tc::maybe_widget_help(include_str!("help.txt"), include_str!("CONFIGURE.md"), true);
    if !tc::dependencies_available(
        "google-trends",
        include_str!("dependencies.json"),
        Some(SETTINGS),
    ) {
        return;
    }
    let cfg = tc::load_config("google_trends");
    let mut refresh = tc::poll_secs(
        tc::cfg_f64(&cfg, "refresh", DEFAULT_REFRESH),
        DEFAULT_REFRESH,
    );
    let mut geos: Vec<String> = tc::cfg_strings(&cfg, "geos", &["US"]);
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut named = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-n" | "--refresh" if i + 1 < args.len() => {
                refresh = tc::poll_secs(
                    args[i + 1].parse().unwrap_or(DEFAULT_REFRESH),
                    DEFAULT_REFRESH,
                );
                i += 2;
            }
            other if !other.starts_with('-') => {
                named.push(other.to_string());
                i += 1;
            }
            _ => i += 1,
        }
    }
    if !named.is_empty() {
        geos = named;
    }
    // Google's codes are upper case; a lower-case one is the same region.
    let mut unique = HashSet::new();
    geos = geos
        .into_iter()
        .map(|g| g.trim().to_uppercase())
        .filter(|g| !g.is_empty() && unique.insert(g.clone()))
        .collect();
    let refresh = refresh.max(MIN_REFRESH);
    let p = palette();

    let shared = Arc::new(Mutex::new(Shared {
        regions: geos
            .iter()
            .map(|g| Region {
                geo: g.clone(),
                ..Region::default()
            })
            .collect(),
        err: String::new(),
    }));
    let activity = tc::Refresh::default();
    let wake = refresh_wake::new();
    if !geos.is_empty() {
        let poller = Arc::clone(&shared);
        let poller_wake = Arc::clone(&wake);
        let poller_activity = activity.clone();
        let poll_geos = geos.clone();
        std::thread::spawn(move || {
            tc::record_panics();
            let mut memories: Vec<(Memory, Option<String>)> =
                poll_geos.iter().map(|g| load_memory(g)).collect();
            if let Ok(mut g) = poller.lock() {
                for (k, (_, note)) in memories.iter().enumerate() {
                    g.regions[k].memory_note = note.clone();
                }
            }
            loop {
                let work = poller_activity.start();
                let round = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    for (k, geo) in poll_geos.iter().enumerate() {
                        let answer = fetch(geo);
                        let now = tc::now();
                        let (memory, note) = &mut memories[k];
                        let got_items = matches!(answer, Answer::Items(_));
                        let mut region = match poller.lock() {
                            Ok(g) => g.regions[k].clone(),
                            Err(_) => return,
                        };
                        apply(&mut region, answer, memory, now, refresh);
                        if got_items {
                            *note = note_after_save(
                                note.take(),
                                save_memory(geo, memory),
                                region.since.is_some(),
                            );
                        }
                        region.memory_note = note.clone();
                        if let Ok(mut g) = poller.lock() {
                            g.regions[k] = region;
                        }
                    }
                }));
                drop(work);
                // Every region's next fetch follows the wait below, which
                // starts now, at the end of the round - not when its own
                // fetch finished, or a slow round reads "retrying now" while
                // the poller is still asleep.
                if round.is_ok() {
                    let due = tc::now() + refresh;
                    if let Ok(mut g) = poller.lock() {
                        for r in g.regions.iter_mut() {
                            r.next_at = due;
                        }
                    }
                }
                if round.is_err() {
                    if let Ok(mut g) = poller.lock() {
                        g.err = tc::take_panic_reason()
                            .map(|why| format!("poller stopped: {why}"))
                            .unwrap_or_else(|| "poller stopped, with no reason recorded".into());
                    }
                    return;
                }
                if refresh_wake::wait_for(&poller_wake, Duration::from_secs_f64(refresh)).is_err() {
                    if let Ok(mut g) = poller.lock() {
                        g.err = "poller stopped: its wake lock was poisoned".to_string();
                    }
                    return;
                }
            }
        });
    }

    tc::setup();
    let mut keyboard = tc::Keyboard::new();
    let mut view = View::default();
    let mut detail: Option<Detail> = None;
    let (mut scroll, mut moved) = (0usize, false);
    let mut placed: Vec<(usize, usize)> = Vec::new();
    let mut slots: Vec<(usize, usize)> = Vec::new();
    let mut room = 1usize;
    let mut note_at = 0.0f64;
    let mut was: Picked = (0, [None, None]);
    const HEAD: usize = 1;

    loop {
        let (w, h) = tc::size();
        let cw = w.saturating_sub(1).max(1);
        let now = tc::now();
        if !view.note.is_empty() && now - note_at > 5.0 {
            view.note.clear();
        }
        let snapshot = match shared.lock() {
            Ok(g) => Shared {
                regions: g.regions.clone(),
                err: g.err.clone(),
            },
            Err(_) => Shared {
                regions: Vec::new(),
                err: "the poller's state was poisoned".into(),
            },
        };
        let several = snapshot.regions.len() > 1;
        // The body as it stands before this frame's keys, for resolving
        // what a key or click refers to.
        let before = list_body(&snapshot, &view, refresh, cw, now, &p);
        follow_titles(&mut view, &snapshot, &before, &was);

        let mut keys = keyboard.poll();
        let current = match &detail {
            Some(d) => Some(d.at),
            None => slots
                .iter()
                .position(|&s| s == (view.focus, view.at[view.focus])),
        };
        if let Some(at) = tc::rows_clicked(&mut keys, current, HEAD, scroll, &placed, None) {
            match &mut detail {
                Some(d) => d.at = at,
                None => {
                    if let Some(&(section, index)) = slots.get(at) {
                        view.focus = section;
                        view.wanted = section;
                        view.at[section] = index;
                    }
                }
            }
            moved = true;
        }
        let mut quit = false;
        for key in keys {
            if key.is_empty() {
                continue;
            }
            // The view keys answer the same on every screen.
            match key.as_str() {
                "ctrl-y" | "wheel-up" => {
                    scroll = scroll.saturating_sub(1);
                    continue;
                }
                "ctrl-e" | "wheel-down" => {
                    scroll += 1;
                    continue;
                }
                "pgup" => {
                    scroll = scroll.saturating_sub(room.max(1));
                    continue;
                }
                "pgdn" => {
                    scroll += room.max(1);
                    continue;
                }
                _ => {}
            }
            if let Some(d) = &mut detail {
                match key.as_str() {
                    "q" | "Q" => quit = true,
                    "esc" | "left" | "backspace" | "h" => {
                        detail = None;
                        scroll = 0;
                        moved = true;
                    }
                    "up" | "k" => {
                        d.at = d.at.saturating_sub(1);
                        moved = true;
                    }
                    "down" | "j" => {
                        if d.at + 1 < d.news.len() {
                            d.at += 1;
                        }
                        moved = true;
                    }
                    "home" => {
                        d.at = 0;
                        scroll = 0;
                    }
                    "end" => {
                        d.at = d.news.len().saturating_sub(1);
                        moved = true;
                    }
                    "c" | "C" => {
                        view.note = match d.news.get(d.at) {
                            Some(n) if !n.url.is_empty() => {
                                if tc::clipboard(&n.url) {
                                    "copied the story's URL".to_string()
                                } else {
                                    format!("no clipboard here; the URL is {}", n.url)
                                }
                            }
                            _ => "this story has no URL to copy".to_string(),
                        };
                        note_at = now;
                    }
                    _ => {}
                }
                continue;
            }
            if view.typing {
                match key.as_str() {
                    "esc" => {
                        view.needle.clear();
                        view.typing = false;
                    }
                    "enter" => view.typing = false,
                    "backspace" => {
                        view.needle.pop();
                    }
                    other if other.chars().count() == 1 => view.needle.push_str(other),
                    _ => {}
                }
                view.at = [0; 3];
                moved = true;
                continue;
            }
            match key.as_str() {
                "q" | "Q" => quit = true,
                "," => {
                    tc::run_settings(&mut keyboard, SETTINGS);
                    continue;
                }
                "/" => {
                    view.typing = true;
                }
                "esc" => {
                    view.needle.clear();
                }
                "r" | "R" => {
                    refresh_wake::request(&wake);
                    view.note = "asking Google again now".to_string();
                    note_at = now;
                }
                "tab" => {
                    if let Some(next) = tc::next_section(Some(view.focus), &before.lens)
                        .or_else(|| tc::next_section(None, &before.lens))
                    {
                        view.focus = next;
                        view.wanted = next;
                        moved = true;
                    }
                }
                "left" | "right" if several => {
                    let n = snapshot.regions.len();
                    view.region = if key == "left" {
                        (view.region + n - 1) % n
                    } else {
                        (view.region + 1) % n
                    };
                    view.at[TRENDING] = 0;
                    view.at[LEFT] = 0;
                    // REGIONS follows, so a fallback into it lands on the
                    // region being shown rather than the first one.
                    view.at[REGIONS] = view.region;
                    scroll = 0;
                }
                "up" | "k" | "down" | "j" => {
                    let down = key == "down" || key == "j";
                    let focus = if before.lens[view.focus] > 0 {
                        Some(view.focus)
                    } else {
                        tc::next_section(None, &before.lens)
                    };
                    if let Some(focus) = focus {
                        let at = if focus == view.focus {
                            view.at[focus]
                        } else {
                            0
                        };
                        if let Some((s, i)) =
                            tc::step_across_sections(focus, at, &before.lens, down)
                        {
                            view.focus = s;
                            view.wanted = s;
                            view.at[s] = i;
                        }
                    }
                    moved = true;
                }
                "home" => {
                    scroll = 0;
                    if let Some(first) = tc::next_section(None, &before.lens) {
                        view.focus = first;
                        view.wanted = first;
                        view.at[first] = 0;
                    }
                }
                "end" => {
                    if let Some(last) = (0..3).rev().find(|&s| before.lens[s] > 0) {
                        view.focus = last;
                        view.wanted = last;
                        view.at[last] = before.lens[last] - 1;
                        moved = true;
                    }
                }
                "enter" => {
                    if view.focus == REGIONS {
                        if let Some(&r) = before.visible[REGIONS].get(view.at[REGIONS]) {
                            view.region = r;
                            view.at[TRENDING] = 0;
                            view.at[LEFT] = 0;
                            scroll = 0;
                        }
                    } else if let Some(d) = detail_for(&snapshot, &view, &before, now) {
                        detail = Some(d);
                        scroll = 0;
                    }
                }
                "c" | "C" => {
                    let region = snapshot.regions.get(view.region);
                    let term = before.visible[view.focus]
                        .get(view.at[view.focus])
                        .and_then(|&i| {
                            let r = region?;
                            match view.focus {
                                TRENDING => r.items.get(i).map(|t| t.title.clone()),
                                LEFT => r.left.get(i).map(|l| l.title.clone()),
                                _ => None,
                            }
                        });
                    view.note = match (term, region) {
                        (Some(term), Some(r)) => {
                            let url = explore_url(&term, &r.geo);
                            if tc::clipboard(&url) {
                                format!("copied the Explore link for {term}")
                            } else {
                                format!("no clipboard here; the link is {url}")
                            }
                        }
                        _ => "select a trend to copy its Explore link".to_string(),
                    };
                    note_at = now;
                }
                _ => {}
            }
        }
        if quit {
            break;
        }

        // Keep the cursor on something that exists after the data moved.
        let body = list_body(&snapshot, &view, refresh, cw, now, &p);
        view.focus = if body.lens[view.wanted] > 0 {
            view.wanted
        } else {
            tc::next_section(None, &body.lens).unwrap_or(view.wanted)
        };
        for s in 0..3 {
            view.at[s] = view.at[s].min(body.lens[s].saturating_sub(1));
        }
        was = picked(&snapshot, &view, &body);
        let region_geo = snapshot
            .regions
            .get(view.region)
            .map(|r| r.geo.clone())
            .unwrap_or_default();
        let title_text = if region_geo.is_empty() {
            "google trends".to_string()
        } else {
            format!("trends {}", region_geo)
        };
        let title = tc::title_busy(
            &title_text,
            w,
            &p.title,
            activity.tick(tc::animation_tick()),
        );
        let (body, hints) = match &detail {
            Some(d) => (detail_body(d, cw, now, &p), detail_hints()),
            None => (
                list_body(&snapshot, &view, refresh, cw, now, &p),
                list_hints(&p, several, &view),
            ),
        };
        let mut body = body;
        if detail.is_some() && !view.note.is_empty() {
            let mut noted = Vec::new();
            for l in tc::wrap_words(&view.note, cw.saturating_sub(1)) {
                noted.extend(line(&p.ok, 1, &l, cw));
            }
            let n = noted.len();
            body.rows.splice(0..0, noted);
            for (row, _) in body.placed.iter_mut() {
                *row += n;
            }
            body.cursor = body.cursor.map(|c| c + n);
        }
        let (packed, foot) = footer(&hints, cw, &p);
        room = h.saturating_sub(HEAD + foot.len()).max(1);
        if moved {
            if let Some(c) = body.cursor {
                scroll = tc::follow(scroll, c, room);
            }
            moved = false;
        }
        let (shown, clamped) = window(&body.rows, scroll, room);
        scroll = clamped;
        placed = body
            .placed
            .iter()
            .map(|&(row, slot)| (row + HEAD, slot))
            .collect();
        slots = body.slots.clone();
        let mut frame = vec![title];
        frame.extend(shown);
        let foot_top = frame.len();
        frame.extend(foot);
        tc::draw(&frame, w, h);
        keyboard.footer_at(&packed, foot_top, 1);
        keyboard.wait(Duration::from_millis(100));
    }
    keyboard.restore();
    tc::restore_screen();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_save_that_works_again_ends_the_warning() {
        assert_eq!(
            note_after_save(None, Err("disk full".into()), false).as_deref(),
            Some("could not save what was seen (disk full)")
        );
        assert_eq!(
            note_after_save(
                Some("could not save what was seen (disk full)".into()),
                Ok(()),
                false
            ),
            None
        );
        // First fetch after starting fresh: nothing compared yet, the
        // notice stays.
        let fresh = || Some("saved history would not read (x), started fresh".to_string());
        assert_eq!(note_after_save(fresh(), Ok(()), false), fresh());
        assert_eq!(note_after_save(fresh(), Ok(()), true), None);
        // A failure wins over every note it meets.
        assert_eq!(
            note_after_save(fresh(), Err("ro".into()), true).as_deref(),
            Some("could not save what was seen (ro)")
        );
    }

    fn trend(title: &str, traffic: &str, started: Option<i64>, sources: &[&str]) -> Trend {
        Trend {
            title: title.into(),
            traffic: traffic.into(),
            started,
            started_raw: String::new(),
            news: sources
                .iter()
                .enumerate()
                .map(|(i, s)| NewsItem {
                    title: format!("A made-up headline number {i} about {title}"),
                    url: format!("https://example.com/news/{i}/a-very-long-path-segment-that-has-no-spaces-at-all"),
                    source: s.to_string(),
                })
                .collect(),
        }
    }

    const NOW: f64 = 1_792_650_000.0;

    fn sample() -> Shared {
        let items = vec![
            trend(
                "zorblax festival",
                "5000+",
                Some(NOW as i64 - 3_600),
                &["Example News", "Sample Gazette"],
            ),
            trend(
                "an exceptionally long made-up search term that has to wrap",
                "200+",
                Some(NOW as i64 - 600),
                &["Example News"],
            ),
            trend("quux", "2M+", None, &[]),
        ];
        let mut memory = Memory::default();
        memory.observe(
            &[
                trend("gone term", "1000+", None, &["Example News"]),
                items[0].clone(),
            ],
            NOW - 600.0,
            f64::INFINITY,
        );
        let mut us = Region {
            geo: "US".into(),
            ..Region::default()
        };
        apply(
            &mut us,
            Answer::Items(items),
            &mut memory,
            NOW - 60.0,
            300.0,
        );
        let mut zz = Region {
            geo: "ZZ".into(),
            ..Region::default()
        };
        apply(
            &mut zz,
            Answer::Rejected("Google rejects geo \"ZZ\"".into()),
            &mut Memory::default(),
            NOW - 60.0,
            300.0,
        );
        let mut gb = Region {
            geo: "GB".into(),
            ..Region::default()
        };
        apply(
            &mut gb,
            Answer::Items(Vec::new()),
            &mut Memory::default(),
            NOW - 60.0,
            300.0,
        );
        Shared {
            regions: vec![us, zz, gb],
            err: String::new(),
        }
    }

    fn plain(rows: &[String]) -> String {
        rows.iter()
            .map(|r| tc::source_text(r))
            .collect::<Vec<_>>()
            .join("\n")
    }

    const WIDTHS: [usize; 5] = [30, 40, 60, 100, 140];

    fn every_screen(w: usize) -> Vec<Vec<String>> {
        let p = palette();
        let cw = w - 1;
        let shared = sample();
        let mut screens = Vec::new();
        for region in 0..shared.regions.len() {
            for focus in [TRENDING, LEFT, REGIONS] {
                let view = View {
                    region,
                    focus,
                    ..View::default()
                };
                screens.push(list_body(&shared, &view, 300.0, cw, NOW, &p).rows);
            }
        }
        let view = View {
            needle: "nothing matches this".into(),
            ..View::default()
        };
        screens.push(list_body(&shared, &view, 300.0, cw, NOW, &p).rows);
        let body = list_body(&shared, &View::default(), 300.0, cw, NOW, &p);
        let d = detail_for(&shared, &View::default(), &body, NOW).unwrap();
        screens.push(detail_body(&d, cw, NOW, &p).rows);
        for hints in [list_hints(&p, true, &View::default()), detail_hints()] {
            screens.push(footer(&hints, cw, &p).1);
        }
        screens
    }

    #[test]
    fn every_row_fits_at_every_width() {
        for w in WIDTHS {
            for rows in every_screen(w) {
                for row in rows {
                    assert!(
                        tc::display_width(&row) <= w,
                        "{w} columns: {:?} is {} wide",
                        tc::source_text(&row),
                        tc::display_width(&row)
                    );
                }
            }
        }
    }

    #[test]
    fn nothing_is_cut_at_any_width() {
        // Every word of every term and every bucket is on screen whole.
        for w in WIDTHS {
            let p = palette();
            let shared = sample();
            let text = plain(&list_body(&shared, &View::default(), 300.0, w - 1, NOW, &p).rows);
            for t in &shared.regions[0].items {
                assert!(text.contains(&t.traffic), "{w}: {} missing", t.traffic);
                for word in t.title.split_whitespace() {
                    assert!(text.contains(word), "{w}: {word} missing from\n{text}");
                }
            }
            // The URL wraps hard; joined back up it is all there.
            let body = list_body(&shared, &View::default(), 300.0, w - 1, NOW, &p);
            let d = detail_for(&shared, &View::default(), &body, NOW).unwrap();
            let joined: String = plain(&detail_body(&d, w - 1, NOW, &p).rows)
                .split_whitespace()
                .collect();
            for n in &d.news {
                assert!(joined.contains(&n.url), "{w}: {} missing", n.url);
            }
        }
    }

    #[test]
    fn news_then_seen_drop_as_the_pane_narrows() {
        let wide = columns(139, 5, 9, 3, 30, 40);
        assert!(wide.news > 0 && wide.seen > 0 && wide.started > 0);
        let mid = columns(45, 5, 9, 3, 30, 40);
        assert_eq!(mid.news, 0);
        assert!(mid.seen > 0);
        let narrow = columns(35, 5, 9, 3, 30, 40);
        assert_eq!((narrow.news, narrow.seen), (0, 0));
        assert!(narrow.started > 0);
        let tiny = columns(29, 5, 9, 3, 30, 40);
        assert_eq!((tiny.news, tiny.seen, tiny.started), (0, 0, 0));
        assert!(tiny.search >= 12 && tiny.traffic >= 5);
        assert!(!tiny.stacked);
        // SEARCH and TRAFFIC side by side need 2 + 6 + 1 + 7 cells.
        assert!(!columns(16, 5, 9, 3, 30, 40).stacked);
        let under = columns(15, 5, 9, 3, 30, 40);
        assert!(under.stacked);
        assert_eq!(under.search, 13);
    }

    #[test]
    fn a_refresh_keeps_the_selected_trend_not_its_row() {
        let p = palette();
        let mut shared = sample();
        let mut view = View::default();
        view.at[TRENDING] = 2; // quux
        let body = list_body(&shared, &view, 300.0, 99, NOW, &p);
        let was = picked(&shared, &view, &body);
        assert_eq!(was.1[0].as_deref(), Some("quux"));
        // Google puts a new term at the top.
        shared.regions[0]
            .items
            .insert(0, trend("brand new", "200+", None, &[]));
        let body = list_body(&shared, &view, 300.0, 99, NOW, &p);
        follow_titles(&mut view, &shared, &body, &was);
        assert_eq!(view.at[TRENDING], 3, "still on quux");
        // A title that has gone keeps the row number.
        shared.regions[0].items.retain(|t| t.title != "quux");
        let body = list_body(&shared, &view, 300.0, 99, NOW, &p);
        follow_titles(&mut view, &shared, &body, &was);
        assert_eq!(view.at[TRENDING], 3);
    }

    #[test]
    fn a_stale_previous_fetch_says_why_nothing_is_compared() {
        let p = palette();
        let mut memory = Memory::default();
        memory.observe(&[trend("alpha", "200+", None, &[])], NOW - 86_400.0, f64::INFINITY);
        let mut us = Region {
            geo: "US".into(),
            ..Region::default()
        };
        apply(
            &mut us,
            Answer::Items(vec![trend("beta", "200+", None, &[])]),
            &mut memory,
            NOW,
            300.0,
        );
        assert!(us.left.is_empty(), "alpha did not leave just now");
        let shared = Shared {
            regions: vec![us],
            err: String::new(),
        };
        let text = plain(&list_body(&shared, &View::default(), 300.0, 140, NOW, &p).rows);
        assert!(text.contains("too long to compare"), "{text}");
        assert!(!text.contains("new since"), "{text}");
    }

    #[test]
    fn a_departure_from_before_a_restart_does_not_claim_google_said_nothing() {
        let p = palette();
        let gone = |news: Option<Vec<NewsItem>>| Left {
            title: "gone term".into(),
            traffic: "1000+".into(),
            first_seen: NOW - 900.0,
            last_seen: NOW - 600.0,
            left_at: NOW - 300.0,
            polls: 2,
            news,
        };
        let text_of = |l: Left| {
            let shared = Shared {
                regions: vec![Region {
                    geo: "US".into(),
                    left: vec![l],
                    ..Region::default()
                }],
                err: String::new(),
            };
            let view = View {
                focus: LEFT,
                wanted: LEFT,
                ..View::default()
            };
            let body = list_body(&shared, &view, 300.0, 99, NOW, &p);
            let d = detail_for(&shared, &view, &body, NOW).unwrap();
            plain(&detail_body(&d, 99, NOW, &p).rows)
        };
        let restarted = text_of(gone(None));
        assert!(restarted.contains("not kept"), "{restarted}");
        assert!(!restarted.contains("Google lists no stories"), "{restarted}");
        assert!(restarted.contains("start time not kept"), "{restarted}");
        assert!(!restarted.contains("no start time in the feed"), "{restarted}");
        let empty = text_of(gone(Some(Vec::new())));
        assert!(empty.contains("Google lists no stories"), "{empty}");
    }

    #[test]
    fn short_terms_still_leave_room_for_the_heading() {
        for cw in [16, 20, 30, 45, 60, 140] {
            let c = columns(cw, 4, 9, 2, 3, 4);
            if !c.stacked {
                assert!(c.search >= "SEARCH".len(), "{cw}: {c:?}");
            }
            if c.news > 0 {
                assert!(c.news >= "NEWS".len(), "{cw}: {c:?}");
            }
        }
    }

    #[test]
    fn a_pane_too_narrow_for_two_columns_stacks_them() {
        let p = palette();
        let shared = sample();
        for w in [8, 10, 12, 15] {
            let rows = list_body(&shared, &View::default(), 300.0, w, NOW, &p).rows;
            for row in &rows {
                assert!(
                    tc::display_width(row) <= w,
                    "{w}: {:?} is too wide",
                    tc::source_text(row)
                );
            }
            let text = plain(&rows);
            for t in &shared.regions[0].items {
                assert!(text.contains(&t.traffic), "{w}: {} missing", t.traffic);
            }
        }
    }

    #[test]
    fn the_bucket_is_shown_verbatim() {
        let p = palette();
        let text = plain(&list_body(&sample(), &View::default(), 300.0, 99, NOW, &p).rows);
        assert!(text.contains("2M+"));
        assert!(text.contains("5000+"));
        assert!(text.contains("a bucket, not a count"));
    }

    #[test]
    fn seen_and_left_are_the_widgets_own_counts() {
        let p = palette();
        let text = plain(&list_body(&sample(), &View::default(), 300.0, 139, NOW, &p).rows);
        assert!(text.contains("2×"), "zorblax was in both fetches\n{text}");
        assert!(text.contains("new"), "{text}");
        assert!(text.contains("2 new since"), "{text}");
        assert!(text.contains("LEFT THE FEED"));
        assert!(text.contains("gone term"));
        assert!(text.contains("was 1000+"));
        assert!(text.contains("this widget's own observation"));
    }

    #[test]
    fn failure_states_each_have_a_named_row() {
        let p = palette();
        let shared = sample();
        let zz = plain(
            &list_body(
                &shared,
                &View {
                    region: 1,
                    ..View::default()
                },
                300.0,
                99,
                NOW,
                &p,
            )
            .rows,
        );
        assert!(zz.contains("Google rejects geo \"ZZ\""), "{zz}");
        assert!(zz.contains(tc::SET_IN_SETTINGS));
        let gb = plain(
            &list_body(
                &shared,
                &View {
                    region: 2,
                    ..View::default()
                },
                300.0,
                99,
                NOW,
                &p,
            )
            .rows,
        );
        assert!(
            gb.contains("Google's feed for GB has no items right now"),
            "{gb}"
        );

        // A failed fetch keeps the last rows behind a banner.
        let mut failing = sample();
        let mut memory = Memory::default();
        apply(
            &mut failing.regions[0],
            Answer::Failed("HTTP 429".into()),
            &mut memory,
            NOW - 10.0,
            300.0,
        );
        let text = plain(&list_body(&failing, &View::default(), 300.0, 99, NOW, &p).rows);
        assert!(text.contains("fetch failed: HTTP 429"), "{text}");
        assert!(text.contains("retrying in"), "{text}");
        assert!(text.contains("zorblax"), "the last rows stay up\n{text}");

        apply(
            &mut failing.regions[0],
            Answer::Unreadable("line 3: expected </item>, found </rss>".into()),
            &mut memory,
            NOW - 5.0,
            300.0,
        );
        let text = plain(&list_body(&failing, &View::default(), 300.0, 99, NOW, &p).rows);
        assert!(text.contains("would not parse: line 3"), "{text}");

        let dead = Shared {
            err: "poller stopped: boom (main.rs:1)".into(),
            ..sample()
        };
        let text = plain(&list_body(&dead, &View::default(), 300.0, 99, NOW, &p).rows);
        assert!(text.contains("poller stopped: boom"), "{text}");

        let waiting = Shared {
            regions: vec![Region {
                geo: "FR".into(),
                ..Region::default()
            }],
            err: String::new(),
        };
        let text = plain(&list_body(&waiting, &View::default(), 300.0, 99, NOW, &p).rows);
        assert!(text.contains("fetching Google's feed for FR"), "{text}");
    }

    #[test]
    fn a_first_check_says_so() {
        let p = palette();
        let mut r = Region {
            geo: "US".into(),
            ..Region::default()
        };
        apply(
            &mut r,
            Answer::Items(vec![trend("alpha", "200+", None, &[])]),
            &mut Memory::default(),
            NOW,
            300.0,
        );
        let shared = Shared {
            regions: vec![r],
            err: String::new(),
        };
        let text = plain(&list_body(&shared, &View::default(), 300.0, 99, NOW, &p).rows);
        assert!(text.contains("first check"), "{text}");
        assert!(!text.contains("new since"), "{text}");
    }

    #[test]
    fn a_filter_is_named_and_an_empty_result_says_why() {
        let p = palette();
        let view = View {
            needle: "zorb".into(),
            ..View::default()
        };
        let text = plain(&list_body(&sample(), &view, 300.0, 99, NOW, &p).rows);
        assert!(text.contains("1 of 3 shown"), "{text}");
        let view = View {
            needle: "nothing matches this".into(),
            ..View::default()
        };
        let text = plain(&list_body(&sample(), &view, 300.0, 99, NOW, &p).rows);
        assert!(text.contains("hidden by the filter"), "{text}");
    }

    #[test]
    fn every_item_row_is_placed_for_clicks() {
        let p = palette();
        let body = list_body(&sample(), &View::default(), 300.0, 59, NOW, &p);
        let slots: HashSet<usize> = body.placed.iter().map(|&(_, s)| s).collect();
        assert_eq!(slots.len(), body.slots.len());
        assert_eq!(body.slots.len(), body.lens.iter().sum::<usize>());
    }

    #[test]
    fn the_explore_link_is_encoded() {
        assert_eq!(
            explore_url("a&b c", "US"),
            "https://trends.google.com/trends/explore?q=a%26b+c&geo=US"
        );
    }

    #[test]
    fn the_region_index_is_shown_when_there_are_several() {
        let p = palette();
        let text = plain(&list_body(&sample(), &View::default(), 300.0, 99, NOW, &p).rows);
        assert!(text.contains("1 of 3"));
        assert!(text.contains("REGIONS"));
        assert!(text.contains("every 5 min"));
    }

    #[test]
    fn palette_clears_aa_on_the_tint() {
        fn channels(esc: &str) -> (f64, f64, f64) {
            let nums: Vec<f64> = esc
                .trim_start_matches('\u{1b}')
                .trim_start_matches('[')
                .trim_end_matches('m')
                .split(';')
                .skip(2)
                .map(|n| n.parse().unwrap())
                .collect();
            (nums[0], nums[1], nums[2])
        }
        fn lum((r, g, b): (f64, f64, f64)) -> f64 {
            let ch = |c: f64| {
                let c = c / 255.0;
                if c <= 0.03928 {
                    c / 12.92
                } else {
                    ((c + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * ch(r) + 0.7152 * ch(g) + 0.0722 * ch(b)
        }
        let p = palette();
        let tint = lum(channels(&p.tint));
        for c in [
            &p.title, &p.head, &p.text, &p.dim, &p.traffic, &p.fresh, &p.ok, &p.warn, &p.bad,
            &p.accent,
        ] {
            let l = lum(channels(c));
            let ratio = (l.max(tint) + 0.05) / (l.min(tint) + 0.05);
            assert!(ratio >= 4.5, "{:?} measures {ratio:.2}", channels(c));
        }
    }
}
