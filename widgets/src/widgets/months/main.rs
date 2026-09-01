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

//! A month grid you can page through, with today in context.
//!
//! `clocks` owns the time of day; this owns dates. Everything on screen is
//! computed from the system clock and the timezone database, which is as
//! real as anything else here.
//!
//! Not called `cal` or `calendar`: both are commands on a normal machine -
//! `cal` ships in util-linux and on macOS, `calendar` is a BSD tool Debian
//! packages - and neither being installed on the box this was written on
//! says nothing about anybody else's.

use std::time::Duration as Wait;

use chrono::{Datelike, Duration, Local, NaiveDate, TimeZone, Utc, Weekday};
use chrono_tz::Tz;
use opscope_core as tc;

const SETTINGS: tc::SettingsSpec = tc::SettingsSpec {
    widget: "months",
    section: "months",
    legacy_section: None,
    schema: include_str!("settings.json"),
    // Nothing here is keyed by something the code owns: three fields, all
    // scalars, and `week_start`'s two answers are named in the schema.
    catalogues: &[],
};

/// Weeks of context kept either side of the week today is in.
const CONTEXT: usize = 2;

/// Months kept either side of the one in view.
///
/// The strip used to begin at the month in view and only go forward, so the
/// month just gone - the one a date is most often checked against - was
/// always a keypress away and never on screen. `CONTEXT` already makes this
/// argument a row at a time; these two make it a month at a time.
///
/// A wide pane still fills with months rather than margins. These are a
/// floor on how many are drawn, not a cap.
const BEFORE: usize = 1;
const AFTER: usize = 1;

/// The rows the grid area keeps, whether or not a month fills them.
///
/// The extension can need eight and never more: a month is four to six weeks
/// tall, and today can only be short of context at one end at a time, so the
/// worst case is a six-week month with today in its first or last row.
/// Holding the area at that height means paging does not move the footer -
/// which is the cost of letting the grid grow, and the reason the extension
/// is not simply always on.
const GRID_ROWS: usize = 8;

/// Columns per day: two digits and a spare, which is where a marker for a
/// day with something on it will go if a calendar feed is ever wired in.
/// Reserved rather than used, so that is an addition instead of a relayout.
const CELL: usize = 3;

/// Columns for the week-number gutter, including the space after it.
const GUTTER: usize = 4;

/// Columns between two months drawn side by side.
const GAP: usize = 3;

/// The narrowest a month can be drawn: seven day cells and nothing else.
const NARROWEST: usize = 7 * CELL;

/// Which day a week begins on.
///
/// Sunday by default, in the code and in `config.example.json` both. It
/// changes only where the row breaks: the week numbers stay ISO, and what
/// counts as a weekend is Saturday and Sunday whichever day a row starts on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum WeekStart {
    Sunday,
    Monday,
}

impl WeekStart {
    fn from_config(cfg: &serde_json::Value) -> WeekStart {
        match tc::cfg_str(cfg, "week_start", "sunday").to_lowercase().as_str() {
            // Named answers only. A prefix would take `monsoon` as Monday,
            // and the contract is that anything unrecognised is Sunday
            // rather than a different grid — a typo in a shared config
            // file should not stop a panel on a wall, or quietly move
            // every row boundary.
            "monday" | "mon" => WeekStart::Monday,
            _ => WeekStart::Sunday,
        }
    }

    fn label(self) -> &'static str {
        match self {
            WeekStart::Sunday => "Sunday",
            WeekStart::Monday => "Monday",
        }
    }

    /// The seven column headings, in the order this start draws them.
    fn headings(self) -> [&'static str; 7] {
        match self {
            WeekStart::Sunday => ["Su", "Mo", "Tu", "We", "Th", "Fr", "Sa"],
            WeekStart::Monday => ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"],
        }
    }
}

/// How far into its own row a date sits.
fn offset(date: NaiveDate, start: WeekStart) -> i64 {
    let from_monday = date.weekday().num_days_from_monday() as i64;
    match start {
        WeekStart::Monday => from_monday,
        WeekStart::Sunday => (from_monday + 1) % 7,
    }
}

/// Saturday and Sunday, whichever day the grid begins on.
fn weekend(date: NaiveDate) -> bool {
    date.weekday().num_days_from_monday() >= 5
}

/// Where "today" is reckoned.
///
/// `clocks` exists because this machine and the people reading it are not in
/// the same zone, and a date has that problem in a sharper form: for several
/// hours a day it is a different date in two places, so a grid quietly using
/// UTC would mark the wrong square every evening on a machine east of it.
enum Zone {
    /// Whatever the machine says, which is what `clocks` shows.
    Machine,
    Named(Tz),
}

impl Zone {
    /// The configured zone, and a sentence when the config named one the
    /// database does not know.
    ///
    /// A zone that does not parse falls back to the machine's - and says so
    /// on screen. Falling back quietly is the failure this codebase spends
    /// its checks on: the grid would be right for somewhere nobody asked
    /// about, and every square would look like an answer.
    fn from_config(cfg: &serde_json::Value) -> (Zone, Option<String>) {
        let asked = tc::cfg_str(cfg, "timezone", "");
        if asked.is_empty() {
            return (Zone::Machine, None);
        }
        match asked.parse::<Tz>() {
            Ok(tz) => (Zone::Named(tz), None),
            Err(_) => (
                Zone::Machine,
                Some(format!(
                    "timezone {:?} is not in the database - reckoned on this machine's zone instead",
                    asked
                )),
            ),
        }
    }

    /// The date it is, there, at this instant.
    fn day(&self, now: chrono::DateTime<Utc>) -> NaiveDate {
        match self {
            Zone::Machine => now.with_timezone(&Local).date_naive(),
            Zone::Named(tz) => now.with_timezone(tz).date_naive(),
        }
    }

    /// What the grid is reckoned in, for the line that says so.
    fn label(&self, now: chrono::DateTime<Utc>) -> String {
        match self {
            Zone::Machine => format!("this machine ({})", utc_offset(&now.with_timezone(&Local))),
            Zone::Named(tz) => {
                format!("{} ({})", tz.name(), utc_offset(&now.with_timezone(tz)))
            }
        }
    }
}

/// The offset from UTC, as a person writes it.
fn utc_offset<T>(when: &chrono::DateTime<T>) -> String
where
    T: TimeZone,
    T::Offset: chrono::Offset,
{
    use chrono::Offset;
    let seconds = when.offset().fix().local_minus_utc();
    let sign = if seconds < 0 { '-' } else { '+' };
    let (hours, minutes) = (seconds.abs() / 3600, (seconds.abs() % 3600) / 60);
    if minutes == 0 {
        format!("UTC{}{}", sign, hours)
    } else {
        format!("UTC{}{}:{:02}", sign, hours, minutes)
    }
}

/// A year and a month, which is what paging moves.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Month {
    year: i32,
    month: u32,
}

impl Month {
    fn of(date: NaiveDate) -> Month {
        Month {
            year: date.year(),
            month: date.month(),
        }
    }

    fn first(self) -> Option<NaiveDate> {
        NaiveDate::from_ymd_opt(self.year, self.month, 1)
    }

    /// This month plus `months`, or nothing when that is past the end of the
    /// calendar itself.
    ///
    /// Paging has no ceiling of its own - the only stop is the date type's
    /// own range, some 262,000 years either way - and at that edge the view
    /// holds still rather than wrapping into a year that would draw as a
    /// perfectly ordinary month somewhere else entirely.
    fn shift(self, months: i64) -> Option<Month> {
        let total = self.year as i64 * 12 + (self.month as i64 - 1) + months;
        let year = total.div_euclid(12);
        if year < i32::MIN as i64 || year > i32::MAX as i64 {
            return None;
        }
        let it = Month {
            year: year as i32,
            month: total.rem_euclid(12) as u32 + 1,
        };
        it.first().map(|_| it)
    }

    fn days(self) -> i64 {
        match (self.first(), self.shift(1).and_then(|m| m.first())) {
            (Some(first), Some(next)) => (next - first).num_days(),
            // The last month the calendar can express has no next month to
            // measure against, and both ends of its range land in a 31-day
            // month - December at the top, January at the bottom.
            _ => 31,
        }
    }

    /// The heading over the grid: the month's name and its year.
    fn heading(self) -> String {
        match self.first() {
            Some(first) => first.format("%B %Y").to_string().to_uppercase(),
            None => String::new(),
        }
    }
}

/// The weeks a month is drawn as, each named by the date it starts on.
///
/// Ordinarily that is the month plus its leading and trailing spill: five or
/// six rows. When `today` falls inside those rows the grid is extended so
/// there are at least `CONTEXT` weeks either side of the row today is in -
/// which a fixed month grid cannot promise, because a first-row today has no
/// weeks above it and a last-row today none below.
///
/// Extended only when today is on screen, deliberately. Two weeks either
/// side of the current week means nothing in a month you have paged to, and
/// growing every month by the same amount would put four weeks of another
/// month around a grid that has no cursor in it.
fn weeks(month: Month, start: WeekStart, today: Option<NaiveDate>) -> Vec<NaiveDate> {
    let Some(first) = month.first() else {
        return Vec::new();
    };
    let Some(last) = first.checked_add_signed(Duration::days(month.days() - 1)) else {
        return Vec::new();
    };
    let Some(mut top) = first.checked_sub_signed(Duration::days(offset(first, start))) else {
        return Vec::new();
    };
    let Some(bottom) = last.checked_sub_signed(Duration::days(offset(last, start))) else {
        return Vec::new();
    };
    let mut count = ((bottom - top).num_days() / 7 + 1) as usize;

    if let Some(day) = today {
        if let Some(row) = day.checked_sub_signed(Duration::days(offset(day, start))) {
            let from_top = (row - top).num_days();
            if from_top >= 0 && from_top / 7 < count as i64 {
                let at = (from_top / 7) as usize;
                let lead = CONTEXT.saturating_sub(at);
                let trail = CONTEXT.saturating_sub(count - 1 - at);
                count += trail;
                // A month at the very start of the calendar has nothing to
                // extend into; it keeps whatever context it can rather than
                // refusing to draw.
                if let Some(higher) = top.checked_sub_signed(Duration::days(7 * lead as i64)) {
                    top = higher;
                    count += lead;
                }
            }
        }
    }

    (0..count)
        .filter_map(|i| top.checked_add_signed(Duration::days(7 * i as i64)))
        .collect()
}

/// The ISO 8601 week a drawn row carries, taken from the Thursday in it.
///
/// ISO weeks are Monday-reckoned and week 1 is the one holding the year's
/// first Thursday, so the Thursday is the day that decides the number - and
/// it is the one day of a row that sits in the same ISO week whichever day
/// the grid starts on. Numbering a Sunday-start row from its Sunday instead
/// would name the week before for six of the row's seven days: the row
/// Sun 27 Dec 2026 to Sat 2 Jan 2027 opens in ISO week 52 and spends the
/// rest of itself in week 53.
fn iso_week(row: NaiveDate) -> Option<u32> {
    (0..7).find_map(|i| {
        let day = row.checked_add_signed(Duration::days(i))?;
        (day.weekday() == Weekday::Thu).then(|| day.iso_week().week())
    })
}

/// How a pane's width is spent: months across, and whether the week-number
/// gutter is one of them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Layout {
    columns: usize,
    gutter: bool,
}

/// What fits in `width`, or nothing when a single month does not.
///
/// Extra width buys months rather than margins, and the gutter is the first
/// thing dropped on the way down - a week number is worth less than the
/// seventh day of the week. Below seven day cells there is no honest grid to
/// draw, so the widget says so instead of drawing six days and a cut.
fn layout(width: usize, want_gutter: bool) -> Option<Layout> {
    let room = width.saturating_sub(2);
    if room < NARROWEST {
        return None;
    }
    // Asked for and affordable. Turning it off is not only a column back: a
    // narrower block means more months fit across, so this has to be decided
    // here rather than blanked out afterwards.
    let gutter = want_gutter && room >= GUTTER + NARROWEST;
    let block = block_width(gutter);
    Some(Layout {
        // A year across is the cap. Past that the strip is showing the same
        // twelve months a reader already has, and the wall's panes are sixty
        // to seventy columns anyway.
        columns: ((room + GAP) / (block + GAP)).clamp(1, 12),
        gutter,
    })
}

fn block_width(gutter: bool) -> usize {
    NARROWEST + if gutter { GUTTER } else { 0 }
}

/// One month as rows of coloured segments, each row exactly one block wide.
///
/// Row 0 is the heading and row 1 the weekday names; the rest is the grid.
fn block(
    month: Month,
    start: WeekStart,
    today: NaiveDate,
    gutter: bool,
    p: &Palette,
) -> Vec<Vec<(String, String)>> {
    let width = block_width(gutter);
    let blank = |n: usize| (tc::NOBG.to_string(), " ".repeat(n));
    let mut rows: Vec<Vec<(String, String)>> = Vec::new();

    rows.push(vec![(p.lbl.clone(), tc::pad(&month.heading(), width))]);

    let mut header: Vec<(String, String)> = Vec::new();
    if gutter {
        header.push((p.dim.clone(), " wk ".to_string()));
    }
    for name in start.headings() {
        header.push((p.lbl.clone(), format!("{:>2} ", name)));
    }
    rows.push(header);

    for row in weeks(month, start, Some(today)) {
        let mine = (0..7).any(|i| {
            row.checked_add_signed(Duration::days(i)) == Some(today)
        });
        let mut cells: Vec<(String, String)> = Vec::new();
        if gutter {
            // The current week's number is lit, so the row today is in is
            // findable from the gutter as well as from the square. It resets
            // the background like every other cell on the row: the gutter of
            // the second month drawn follows the last square of the first,
            // and if that square is today the tint would run into it.
            let ink = if mine { &p.today } else { &p.dim };
            cells.push((
                format!("{}{}", tc::NOBG, ink),
                match iso_week(row) {
                    Some(n) => format!("{:>3} ", n),
                    None => " ".repeat(GUTTER),
                },
            ));
        }
        for i in 0..7 {
            let Some(day) = row.checked_add_signed(Duration::days(i)) else {
                cells.push(blank(CELL));
                continue;
            };
            let here = day == today;
            // Every cell carries a background of its own: the tint on today
            // and an explicit "no background" everywhere else, because a
            // background escape runs to the end of the row until something
            // resets it - and here the row continues into the next month.
            let tint = if here {
                tc::bg(28, 44, 62)
            } else {
                tc::NOBG.to_string()
            };
            let c = |colour: &str| format!("{}{}", tint, colour);
            let outside = day.month() != month.month || day.year() != month.year;
            let ink = if here {
                c(&p.today)
            } else if outside {
                c(&p.spill)
            } else if weekend(day) {
                c(&p.wknd)
            } else {
                c(&p.txt)
            };
            cells.push((ink, format!("{:>2} ", day.day())));
        }
        rows.push(cells);
    }
    rows
}

/// Every month on show, side by side, as rows of segments.
///
/// The grid area is `GRID_ROWS` tall whatever the months in it need, so the
/// rows below the strip do not move as you page - a pane whose footer walks
/// up and down while you look for a date is a pane you stop trusting. A
/// month that somehow wanted more would get it: the area is a floor, not a
/// cut.
fn strip(
    months: &[Month],
    start: WeekStart,
    today: NaiveDate,
    l: Layout,
    p: &Palette,
) -> Vec<Vec<(String, String)>> {
    let width = block_width(l.gutter);
    let blocks: Vec<Vec<Vec<(String, String)>>> = months
        .iter()
        .map(|m| block(*m, start, today, l.gutter, p))
        .collect();
    let tall = blocks
        .iter()
        .map(|b| b.len())
        .max()
        .unwrap_or(0)
        .max(2 + GRID_ROWS);
    (0..tall)
        .map(|i| {
            let mut row: Vec<(String, String)> = vec![(tc::NOBG.to_string(), " ".to_string())];
            for (n, b) in blocks.iter().enumerate() {
                if n > 0 {
                    row.push((tc::NOBG.to_string(), " ".repeat(GAP)));
                }
                match b.get(i) {
                    Some(cells) => row.extend(cells.iter().cloned()),
                    None => row.push((tc::NOBG.to_string(), " ".repeat(width))),
                }
            }
            row
        })
        .collect()
}

/// A sentence as rows that fit `width`, breaking between words.
///
/// `pack_hints` is what wraps a footer without splitting a hint, and a word
/// is the same shape of thing. A word wider than the pane is broken rather
/// than handed over whole: the terminal would wrap it itself, and a row that
/// wraps is a row the frame did not count - which scrolls the pinned title
/// off the top of the pane. Zone names reach 30 characters with the offset
/// beside them.
fn wrapped(text: &str, colour: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut words: Vec<Vec<(&str, String)>> = Vec::new();
    for word in text.split_whitespace() {
        let mut left: Vec<char> = word.chars().collect();
        while left.len() > width {
            words.push(vec![(colour, left.drain(..width).collect())]);
        }
        words.push(vec![(colour, left.into_iter().collect())]);
    }
    tc::pack_hints(&words, width, " ")
        .into_iter()
        .map(|line| format!(" {}", line))
        .collect()
}

/// The lines under the title: clauses across a row while they fit whole, and
/// one wrapped clause per row on a pane too narrow to join them.
///
/// Everything here answers a question about the grid rather than decorating
/// it - which zone the dates are reckoned in, which day the weeks start on,
/// what the numbers in the gutter count - so none of it may be dropped or
/// cut on the way down. A 26-column pane used to read "2026-08-29 SATURDAY ·
/// r", which is a widget claiming to say what it is showing and not saying
/// it.
fn reckoning(clauses: &[(&str, String)], width: usize) -> Vec<String> {
    if clauses.iter().all(|(_, t)| t.chars().count() <= width) {
        let groups: Vec<Vec<(&str, String)>> =
            clauses.iter().map(|clause| vec![clause.clone()]).collect();
        return tc::pack_hints(&groups, width, " · ")
            .into_iter()
            .map(|line| format!(" {}", line))
            .collect();
    }
    clauses
        .iter()
        .flat_map(|(colour, text)| wrapped(text, colour, width))
        .collect()
}

/// Move the view by `months`, holding still at the calendar's own limit.
///
/// `None` means "the month today is in", which is what the view starts as
/// and what `t` returns it to: a pane left open over the turn of a month
/// then follows the clock instead of sitting on the month it was started in.
///
/// A month is refused when its grid cannot be drawn as well as when the month
/// itself cannot be expressed - at the very bottom of the calendar a January
/// has no room for the leading spill its first row needs, and paging onto it
/// would show a heading over an empty grid, which reads as a widget that has
/// broken rather than as the end of the calendar.
fn page(anchor: Option<Month>, today: NaiveDate, months: i64, start: WeekStart) -> Option<Month> {
    let from = anchor.unwrap_or_else(|| Month::of(today));
    let onto = from
        .shift(months)
        .filter(|m| !weeks(*m, start, None).is_empty());
    Some(onto.unwrap_or(from))
}

/// The footer, which is also where the keys are taught.
fn hints(p: &Palette, neighbours: bool) -> Vec<Vec<(&str, String)>> {
    vec![
        vec![(p.head.as_str(), "←→".into()), (p.dim.as_str(), " month".into())],
        vec![(p.head.as_str(), "↑↓".into()), (p.dim.as_str(), " year".into())],
        vec![(p.dim.as_str(), "[t]oday".into())],
        // Named for where it goes, not for what is on: a footer that reads
        // "[s]ingle" while a single month is showing says nothing about
        // which way the key moves.
        vec![(
            p.dim.as_str(),
            // Both inside twelve characters, because `pack_hints` will not
            // split a hint and the frame promises no row wider than the
            // pane from twelve columns up. The widest hint is the floor.
            if neighbours {
                "[s]ingle".into()
            } else {
                "[s]pread".into()
            },
        )],
        vec![(p.dim.as_str(), "[,] settings".into())],
        vec![(p.dim.as_str(), "[q]uit".into())],
    ]
}

/// The footer as it is drawn: wrapped, and indented while there is room.
///
/// A hint is never split - `[,] settin` teaches a key that does not exist -
/// so on a pane narrower than the widest hint it is the margin that gives
/// way, not the hint. Every other row of the frame keeps its space.
fn footer(w: usize, neighbours: bool, p: &Palette) -> Vec<String> {
    let hints = hints(p, neighbours);
    let widest = hints
        .iter()
        .map(|hint| hint.iter().map(|(_, t)| t.chars().count()).sum::<usize>())
        .max()
        .unwrap_or(0);
    let indent = usize::from(widest + 1 <= w);
    tc::pack_hints(&hints, w.saturating_sub(indent + 1).max(1), "  ")
        .into_iter()
        .map(|line| format!("{}{}", " ".repeat(indent), line))
        .collect()
}

/// The whole frame at width `w`, title first, at whatever height it needs.
///
/// Built as tall as it wants to be and windowed by the caller, so nothing is
/// left undrawn because a pane is short - a grid that is not there looks
/// exactly like a grid with nothing in it.
#[allow(clippy::too_many_arguments)]
fn frame(
    w: usize,
    today: NaiveDate,
    view: Month,
    start: WeekStart,
    zone: &str,
    note: Option<&str>,
    // Whether the months either side are drawn, or this one on its own, and
    // whether the week-number gutter is wanted at all.
    neighbours: bool,
    weeks: bool,
    p: &Palette,
) -> Vec<String> {
    let mut body = vec![tc::title("months", w, &p.head)];
    // Clause by clause rather than one string, so a narrow pane wraps the
    // reckoning instead of cutting it in half - which is where the reader is
    // told which zone and which week numbering this grid is.
    let mut said: Vec<(&str, String)> = vec![
        (
            p.txt.as_str(),
            format!(
                "{} {}",
                today.format("%Y-%m-%d"),
                today.format("%A").to_string().to_uppercase()
            ),
        ),
        (p.dim.as_str(), format!("reckoned on {}", zone)),
        (p.dim.as_str(), format!("weeks start {}", start.label())),
    ];
    // Said in full rather than left to the `wk` heading: ISO weeks are
    // Monday-reckoned whatever this grid does, so on a Sunday-start grid the
    // number belongs to a week beginning a day earlier, and at the turn of a
    // year the two genuinely disagree. An unlabelled number that changes
    // meaning with a config key is the thing that is not allowed - and when
    // the pane is too narrow for the gutter there are no numbers to explain.
    if layout(w, weeks).is_some_and(|l| l.gutter) {
        said.push((
            p.dim.as_str(),
            "wk is the ISO 8601 week, Monday-reckoned".into(),
        ));
    }
    body.extend(reckoning(&said, w.saturating_sub(2)));
    if let Some(note) = note {
        body.extend(wrapped(note, &p.wknd, w.saturating_sub(2)));
    }
    body.push(String::new());

    match layout(w, weeks) {
        None => body.extend(wrapped(
            &format!(
                "a month needs {} columns to draw seven days; this pane has {}",
                NARROWEST + 2, w
            ),
            &p.wknd,
            w.saturating_sub(2),
        )),
        Some(l) => {
            // The month either side at minimum, and more when the width is
            // there to hold them: extra width still buys months, it just
            // cannot buy fewer than the three this widget is for.
            // With the neighbours off it is this month and nothing else,
            // however wide the pane: the point of asking is to see one month
            // on its own, and filling the width with more would be the
            // widget declining to do the thing the key was pressed for.
            let (before, least) = if neighbours {
                (BEFORE, (BEFORE + 1 + AFTER).max(l.columns))
            } else {
                (0, 1)
            };
            let months: Vec<Month> = (0..least)
                .filter_map(|i| view.shift(i as i64 - before as i64))
                .collect();
            // Bands of whatever fits across. A pane too narrow for three
            // months stacks them rather than dropping them and scrolls to
            // reach the rest, which is the rule the rest of this collection
            // follows: a pane too short is a pane you scroll, not a pane
            // that hides things.
            for (n, band) in months.chunks(l.columns.max(1)).enumerate() {
                if n > 0 {
                    body.push(String::new());
                }
                for row in strip(band, start, today, l, p) {
                    let refs: Vec<(&str, String)> =
                        row.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
                    body.push(tc::seg(&refs, w.saturating_sub(1)));
                }
            }
        }
    }
    body
}

fn main() {
    tc::maybe_widget_help(include_str!("help.txt"), include_str!("CONFIGURE.md"), true);
    if !tc::dependencies_available("months", include_str!("dependencies.json"), Some(SETTINGS)) {
        return;
    }
    let cfg = tc::load_config("months");
    let start = WeekStart::from_config(&cfg);
    let (zone, zone_note) = Zone::from_config(&cfg);
    let p = palette();
    tc::setup();
    let mut keyboard = tc::Keyboard::new();
    let mut anchor: Option<Month> = None;
    let mut scroll = 0usize;
    // Whether the months either side are drawn. A view rather than a
    // setting: it is the answer to "just this month, quickly", which is a
    // thing you want for a moment and not a thing you configure.
    let mut neighbours = true;
    // Whether the week-number gutter is wanted at all. Config, not a key:
    // ISO week numbers are either part of how you work or they are noise,
    // and that does not change between one glance and the next.
    let weeks = cfg
        .get("week_numbers")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    loop {
        let now = Utc::now();
        let today = zone.day(now);
        for key in keyboard.poll() {
            match key.as_str() {
                // The shared screen, which relaunches this binary if it
                // wrote anything - so the week start and the zone below are
                // read again rather than held from the launch.
                "," => {
                    tc::run_settings(&mut keyboard, SETTINGS);
                    continue;
                }
                "q" | "Q" => {
                    keyboard.restore();
                    tc::restore_screen();
                    return;
                }
                "left" | "h" | "H" => anchor = page(anchor, today, -1, start),
                "right" | "l" | "L" => anchor = page(anchor, today, 1, start),
                "up" | "k" | "K" | "pgup" => anchor = page(anchor, today, -12, start),
                "down" | "j" | "J" | "pgdn" => anchor = page(anchor, today, 12, start),
                "t" | "T" | "home" => anchor = None,
                // The scroll goes back to the top with it: the body it was
                // measured against has just changed height, and a view left
                // scrolled into space that no longer exists reads as an
                // empty widget.
                "s" | "S" => {
                    neighbours = !neighbours;
                    scroll = 0;
                }
                // The wheel moves the view and nothing else: there is no
                // selection here to protect, but paging is what the keys do
                // and a wheel that paged would step a month per notch.
                "ctrl-y" | "wheel-up" => scroll = scroll.saturating_sub(1),
                "ctrl-e" | "wheel-down" => scroll = scroll.saturating_add(1),
                _ => {}
            }
        }

        let (w, h) = tc::size();
        let view = anchor.unwrap_or_else(|| Month::of(today));
        let body = frame(w, today, view, start, &zone.label(now), zone_note.as_deref(), neighbours, weeks, &p);
        let foot = footer(w, neighbours, &p);
        // A window onto the body rather than a cut of it, with the title
        // pinned above: on a wall of panes it is the only row saying which
        // widget this is.
        let room = h.saturating_sub(foot.len());
        let (head, rest) = body.split_at(1.min(body.len()));
        let room_below = room.saturating_sub(head.len()).max(1);
        scroll = scroll.min(rest.len().saturating_sub(room_below));
        let last = (scroll + room_below).min(rest.len());
        let mut rows: Vec<String> = head.to_vec();
        rows.extend_from_slice(&rest[scroll..last]);
        while rows.len() < room {
            rows.push(String::new());
        }
        rows.extend(foot);
        tc::draw(&rows, w, h);
        std::thread::sleep(Wait::from_millis(200));
    }
}

struct Palette {
    /// The title rule and the arrow hints.
    head: String,
    /// Today's square and the number of the week it is in. 11.70 on the
    /// today tint, 15.22 on the backdrop.
    today: String,
    /// A day of the month on show. 11.77 on the tint, 15.31 on the backdrop.
    txt: String,
    /// Saturday and Sunday, and the two notes that have to be read rather
    /// than glanced at. 8.34 and 10.85.
    wknd: String,
    /// A day belonging to the month either side of this one: dimmed, and
    /// measured rather than eyeballed, because "dimmed" is the whole
    /// requirement and an unreadable grey meets it by accident. 5.85 on the
    /// tint, 7.61 on the backdrop.
    spill: String,
    /// The month heading and the weekday names. 5.52 and 7.18.
    lbl: String,
    /// The week numbers, the reckoning lines and the footer. 4.51 on the
    /// tint, 5.87 on the backdrop - the lowest here, and it clears AA.
    dim: String,
}

fn palette() -> Palette {
    Palette {
        head: tc::rgb(150, 210, 255),
        today: tc::rgb(140, 255, 205),
        txt: tc::rgb(225, 235, 245),
        wknd: tc::rgb(240, 190, 120),
        spill: tc::rgb(140, 170, 195),
        lbl: tc::rgb(130, 165, 200),
        dim: tc::rgb(127, 147, 172),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).expect("a real date")
    }

    /// Which drawn row a date lands in, and how many rows there are.
    fn placed(month: Month, start: WeekStart, today: NaiveDate) -> (usize, usize) {
        let rows = weeks(month, start, Some(today));
        let at = rows
            .iter()
            .position(|row| {
                (0..7).any(|i| row.checked_add_signed(Duration::days(i)) == Some(today))
            })
            .expect("today is somewhere in its own month");
        (at, rows.len())
    }

    #[test]
    fn two_weeks_are_visible_either_side_of_today_wherever_it_falls() {
        // The requirement a fixed month grid cannot meet: today in the top
        // row has nothing above it and today in the bottom row nothing
        // below. Every day of every month of eight years, both week starts -
        // which covers every shape a month can be, leap Februaries included.
        for year in 2024..2032 {
            for month in 1..=12 {
                let m = Month { year, month };
                for d in 1..=m.days() as u32 {
                    let today = day(year, month, d);
                    for start in [WeekStart::Sunday, WeekStart::Monday] {
                        let (at, rows) = placed(m, start, today);
                        assert!(
                            at >= CONTEXT,
                            "{} start {:?}: only {} weeks above today",
                            today,
                            start,
                            at
                        );
                        assert!(
                            rows - 1 - at >= CONTEXT,
                            "{} start {:?}: only {} weeks below today",
                            today,
                            start,
                            rows - 1 - at
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn an_extended_grid_never_outgrows_the_rows_the_pane_keeps() {
        // GRID_ROWS is what holds the footer still while you page, so the
        // claim behind it - eight is the worst case - has to be measured
        // rather than reasoned about once and forgotten.
        let mut tallest = 0;
        for year in 1990..2040 {
            for month in 1..=12 {
                let m = Month { year, month };
                for d in 1..=m.days() as u32 {
                    for start in [WeekStart::Sunday, WeekStart::Monday] {
                        let rows = weeks(m, start, Some(day(year, month, d))).len();
                        tallest = tallest.max(rows);
                        assert!(rows <= GRID_ROWS, "{}-{} needed {} rows", year, month, rows);
                    }
                }
            }
        }
        assert_eq!(tallest, GRID_ROWS, "the reserved height is now more than any month uses");
    }

    #[test]
    fn a_month_today_is_not_in_is_an_ordinary_grid() {
        // The other half of the decision: two weeks either side of the
        // current week means nothing in a month you have paged to, so those
        // draw with their ordinary spill and no more.
        let march = Month { year: 2026, month: 3 };
        let plain = weeks(march, WeekStart::Sunday, Some(day(2026, 8, 29)));
        assert_eq!(plain.len(), 5);
        assert_eq!(plain[0], day(2026, 3, 1));
        // And with today in it, the same month grows around today's row.
        let extended = weeks(march, WeekStart::Sunday, Some(day(2026, 3, 3)));
        assert_eq!(extended.len(), 7);
        assert_eq!(extended[0], day(2026, 2, 15));
        assert!(extended.contains(&day(2026, 3, 1)));
    }

    #[test]
    fn today_in_a_neighbouring_months_spill_still_gets_its_context() {
        // 1 August 2026 is a Saturday and sits in July's last drawn row.
        // Today is on screen in that block, so the requirement applies to it
        // - the rule is "today is in view", not "this is today's month".
        let july = Month { year: 2026, month: 7 };
        let (at, rows) = placed(july, WeekStart::Sunday, day(2026, 8, 1));
        assert!(at >= CONTEXT && rows - 1 - at >= CONTEXT, "row {} of {}", at, rows);
    }

    #[test]
    fn the_week_number_is_iso_whichever_day_the_week_starts() {
        // The trap. ISO 8601 counts weeks from Monday, and the year turn is
        // where an unlabelled number would change meaning under the config
        // key: 27 December 2026 is a Sunday in ISO week 52, and the six days
        // after it are ISO week 53.
        assert_eq!(day(2026, 12, 27).iso_week().week(), 52);
        assert_eq!(day(2026, 12, 31).iso_week().week(), 53);
        // A Sunday-start row opening on that Sunday, and the Monday-start row
        // beside it, carry the same number - because both are numbered from
        // the Thursday, which is in the same ISO week either way.
        assert_eq!(iso_week(day(2026, 12, 27)), Some(53));
        assert_eq!(iso_week(day(2026, 12, 28)), Some(53));
        // And an ordinary week is simply its own number.
        assert_eq!(iso_week(day(2026, 8, 23)), Some(35));
        assert_eq!(iso_week(day(2026, 8, 24)), Some(35));
        // The first week of a year that ISO gives to the year before.
        assert_eq!(iso_week(day(2020, 12, 27)), Some(53));
    }

    #[test]
    fn every_drawn_row_numbers_itself() {
        // Every row of every month for fifty years has a Thursday in it, so
        // there is no row the gutter has to leave blank.
        for year in 1990..2040 {
            for month in 1..=12 {
                for row in weeks(Month { year, month }, WeekStart::Sunday, None) {
                    let n = iso_week(row).unwrap_or(0);
                    assert!((1..=53).contains(&n), "{} numbered {}", row, n);
                }
            }
        }
    }

    #[test]
    fn week_start_reads_from_config_and_defaults_to_sunday() {
        // Sunday in the code and Sunday in config.example.json, which
        // check.rs holds to each other from both directions.
        assert_eq!(WeekStart::from_config(&serde_json::json!({})), WeekStart::Sunday);
        assert_eq!(
            WeekStart::from_config(&serde_json::json!({"week_start": "monday"})),
            WeekStart::Monday
        );
        assert_eq!(
            WeekStart::from_config(&serde_json::json!({"week_start": "MON"})),
            WeekStart::Monday
        );
        // Anything else is the default rather than a stopped panel —
        // including a typo that merely begins with `mon`.
        for junk in ["tuesday", "", "nonsense", "mondy", "monsoon", "monkey"] {
            assert_eq!(
                WeekStart::from_config(&serde_json::json!({"week_start": junk})),
                WeekStart::Sunday,
                "{:?}",
                junk
            );
        }
        assert_eq!(
            WeekStart::from_config(&serde_json::json!({"week_start": 3})),
            WeekStart::Sunday
        );
    }

    #[test]
    fn the_week_start_moves_the_row_break_and_nothing_else() {
        let august = Month { year: 2026, month: 8 };
        let sunday = weeks(august, WeekStart::Sunday, None);
        let monday = weeks(august, WeekStart::Monday, None);
        assert_eq!(sunday[0].weekday(), Weekday::Sun);
        assert_eq!(monday[0].weekday(), Weekday::Mon);
        // The weekend is Saturday and Sunday either way, which is what
        // clocks reckons too - so the new default contradicts nothing.
        assert!(weekend(day(2026, 8, 29)) && weekend(day(2026, 8, 30)));
        assert!(!weekend(day(2026, 8, 31)));
    }

    #[test]
    fn today_is_the_date_where_the_grid_is_reckoned() {
        // Half past four in the afternoon in London on 29 August is already
        // the 30th in Auckland, and a widget quietly using one zone marks the
        // wrong square for everybody in the other.
        let evening = Utc.with_ymd_and_hms(2026, 8, 29, 16, 30, 0).unwrap();
        let london = Zone::Named("Europe/London".parse().unwrap());
        let auckland = Zone::Named("Pacific/Auckland".parse().unwrap());
        assert_eq!(london.day(evening), day(2026, 8, 29));
        assert_eq!(auckland.day(evening), day(2026, 8, 30));
        // And the zone is named on screen, offset and all.
        assert_eq!(london.label(evening), "Europe/London (UTC+1)");
        assert!(Zone::Machine.label(evening).contains("this machine"));
    }

    #[test]
    fn a_zone_the_database_does_not_know_says_so_rather_than_guessing() {
        let (zone, note) = Zone::from_config(&serde_json::json!({"timezone": "Middle/Earth"}));
        assert!(matches!(zone, Zone::Machine));
        let note = note.expect("a sentence naming the setting");
        assert!(note.contains("Middle/Earth"), "{}", note);
        // A configured zone that is real is used, with nothing to say.
        let (zone, note) = Zone::from_config(&serde_json::json!({"timezone": "Asia/Tokyo"}));
        assert!(matches!(zone, Zone::Named(_)));
        assert!(note.is_none());
        // And no setting at all is the machine's own zone, silently.
        let (zone, note) = Zone::from_config(&serde_json::json!({}));
        assert!(matches!(zone, Zone::Machine));
        assert!(note.is_none());
    }

    #[test]
    fn paging_walks_by_month_and_by_year_in_both_directions() {
        let today = day(2026, 8, 29);
        let start = WeekStart::Sunday;
        let mut at = None;
        for _ in 0..5 {
            at = page(at, today, 1, start);
        }
        assert_eq!(at, Some(Month { year: 2027, month: 1 }), "five months on");
        for _ in 0..5 {
            at = page(at, today, -1, start);
        }
        assert_eq!(at, Some(Month { year: 2026, month: 8 }));
        assert_eq!(page(at, today, 12, start), Some(Month { year: 2027, month: 8 }));
        assert_eq!(page(at, today, -12, start), Some(Month { year: 2025, month: 8 }));
        // t goes back to whatever month it is now, rather than to the month
        // the widget was started in.
        assert_eq!(page(None, today, 0, start), Some(Month::of(today)));
    }

    #[test]
    fn paging_has_no_ceiling_this_side_of_the_calendars_own() {
        // "Indefinitely" measured: a couple of hundred years a month at a
        // time, then run at the calendar's own ends a hundred thousand years
        // at a time until it stops - and whatever it stops on has to be a
        // month that draws, not a heading over an empty grid.
        let today = day(2026, 8, 29);
        let start = WeekStart::Sunday;
        let mut at = page(None, today, 0, start);
        for _ in 0..2_000 {
            at = page(at, today, 1, start);
            assert!(at.and_then(|m| m.first()).is_some());
        }
        assert_eq!(at, Some(Month { year: 2193, month: 4 }));

        for (step, edge) in [(-12i64, NaiveDate::MIN), (12, NaiveDate::MAX)] {
            let mut at = page(None, today, 0, start);
            // Enough presses of one key to cross the quarter of a million
            // years the date type has, in either direction.
            for _ in 0..300_000 {
                let next = page(at, today, step, start);
                if next == at {
                    break;
                }
                at = next;
            }
            let stopped = at.expect("a month");
            // It holds still at the end rather than wrapping into a year
            // that would draw as a perfectly ordinary month somewhere else.
            assert_eq!(page(at, today, step, start), at, "{:?} kept moving", stopped);
            assert!(!weeks(stopped, start, None).is_empty(), "{:?} draws nothing", stopped);
            // And it got all the way there: the year it stops in is the last
            // year there is. At the bottom that is February rather than
            // January, which has no room for the spill its first row needs.
            assert_eq!(stopped.year, Month::of(edge).year, "stopped at {:?}", stopped);
        }
        assert_eq!(Month::of(NaiveDate::MAX).shift(1), None);
        assert_eq!(Month::of(NaiveDate::MIN).shift(-1), None);
    }

    #[test]
    fn a_month_is_as_long_as_it_actually_is() {
        assert_eq!(Month { year: 2026, month: 2 }.days(), 28);
        assert_eq!(Month { year: 2028, month: 2 }.days(), 29);
        assert_eq!(Month { year: 2026, month: 4 }.days(), 30);
        assert_eq!(Month { year: 2026, month: 12 }.days(), 31);
        assert_eq!(Month { year: 1900, month: 2 }.days(), 28, "1900 was not a leap year");
        assert_eq!(Month { year: 2000, month: 2 }.days(), 29, "2000 was");
        // The last month the calendar can express has no next month to
        // measure against, and it is a December - taken from the date type
        // rather than written out, because the limit is its to decide and it
        // is not the year this was first written against.
        assert_eq!(Month::of(NaiveDate::MAX).days(), 31);
    }

    #[test]
    fn a_narrow_pane_stacks_the_months_rather_than_dropping_them() {
        // The strip is at least the month either side of the one in view,
        // however narrow the pane. It used to draw exactly as many months as
        // fitted across, so at one column the month just gone - the one a
        // date is most often checked against - simply was not there.
        let today = day(2026, 8, 29);
        let view = Month::of(today);
        // Named outright rather than derived from BEFORE and AFTER. Written
        // in terms of those constants this passed with BEFORE at 0, which is
        // the bug it exists to catch: an assertion that moves with the thing
        // it is checking is checking nothing.
        let gone = view.shift(-1).expect("the month before");
        let next = view.shift(1).expect("the month after");

        for width in [30usize, 64, 90, 400] {
            let l = layout(width, true).expect("a month fits");
            let months: Vec<Month> = (0..(BEFORE + 1 + AFTER).max(l.columns))
                .filter_map(|i| view.shift(i as i64 - BEFORE as i64))
                .collect();
            for (what, m) in [("the month just gone", gone), ("the one in view", view), ("the month ahead", next)] {
                assert!(
                    months.contains(&m),
                    "{width} columns: {what} is not on screen — drew {months:?}"
                );
            }
            // Wide panes still fill across rather than stacking needlessly.
            assert!(months.len() >= l.columns, "at {width} columns");
        }
    }

    #[test]
    fn extra_width_buys_months_and_the_gutter_goes_last() {
        // Sixty to seventy columns is what the panes on the wall are.
        assert_eq!(layout(64, true), Some(Layout { columns: 2, gutter: true }));
        assert_eq!(layout(70, true), Some(Layout { columns: 2, gutter: true }));
        assert_eq!(layout(90, true), Some(Layout { columns: 3, gutter: true }));
        assert_eq!(layout(30, true), Some(Layout { columns: 1, gutter: true }));
        // One month with its gutter needs 27 columns; below that the week
        // numbers go and the seven days stay.
        assert_eq!(layout(26, true), Some(Layout { columns: 1, gutter: false }));
        assert_eq!(layout(23, true), Some(Layout { columns: 1, gutter: false }));
        // And below seven day cells there is no grid to draw.
        assert_eq!(layout(22, true), None);
        assert_eq!(layout(8, true), None);
        // A whole year across, and no more however wide the pane gets.
        assert_eq!(layout(400, true).map(|l| l.columns), Some(12));
        // Whatever it says fits, fits - nothing is truncated into place.
        for w in 23..400usize {
            let l = layout(w, true).expect("a grid");
            let used = l.columns * block_width(l.gutter) + (l.columns - 1) * GAP;
            assert!(used + 1 <= w, "width {} drew {} cells", w, used);
        }
    }

    #[test]
    fn every_row_of_the_strip_is_the_same_width() {
        // A ragged block puts the second month's Tuesdays under the first
        // month's Wednesdays, which is a grid that lies about what day it is.
        let p = palette();
        let today = day(2026, 8, 29);
        for w in [23usize, 30, 64, 90, 140] {
            let l = layout(w, true).expect("a grid");
            let months: Vec<Month> = (0..l.columns)
                .filter_map(|i| Month { year: 2026, month: 8 }.shift(i as i64))
                .collect();
            let rows = strip(&months, WeekStart::Sunday, today, l, &p);
            let widths: Vec<usize> = rows
                .iter()
                .map(|r| r.iter().map(|(_, t)| t.chars().count()).sum())
                .collect();
            assert!(
                widths.windows(2).all(|pair| pair[0] == pair[1]),
                "width {}: rows measured {:?}",
                w,
                widths
            );
            assert_eq!(rows.len(), 2 + GRID_ROWS, "the grid area moved at width {}", w);
        }
    }

    #[test]
    fn the_grid_area_is_the_same_height_whatever_month_is_on_show() {
        // Trap 2 of the issue: a pane whose row count moves as you page is
        // jarring, so the extension grows into a reserved area rather than
        // pushing the footer about.
        let p = palette();
        let today = day(2026, 8, 29);
        let l = layout(64, true).expect("a grid");
        let mut heights = Vec::new();
        for delta in -6..=6 {
            let view = Month::of(today).shift(delta).expect("a month");
            let months: Vec<Month> = (0..l.columns)
                .filter_map(|i| view.shift(i as i64))
                .collect();
            heights.push(strip(&months, WeekStart::Sunday, today, l, &p).len());
        }
        assert!(heights.iter().all(|n| *n == 2 + GRID_ROWS), "{:?}", heights);
    }

    #[test]
    fn today_is_marked_and_the_days_around_it_are_not() {
        let p = palette();
        let today = day(2026, 8, 29);
        let rows = block(Month { year: 2026, month: 8 }, WeekStart::Sunday, today, true, &p);
        let tinted: Vec<&(String, String)> = rows
            .iter()
            .flatten()
            .filter(|(colour, _)| colour.contains("48;2;28;44;62"))
            .collect();
        assert_eq!(tinted.len(), 1, "exactly one square is today");
        assert_eq!(tinted[0].1.trim(), "29");
        assert!(tinted[0].0.contains(&p.today), "today's square is not lit");
        // Every cell of a grid row resets the background, or the tint runs
        // along the row and into the month drawn beside it. The heading and
        // the weekday names are not grid rows and no tint precedes them.
        for row in rows.iter().skip(2) {
            for (colour, text) in row {
                assert!(
                    colour.contains("48;2;") || colour.contains(tc::NOBG),
                    "{:?} neither tints nor resets the background",
                    text
                );
            }
        }
    }

    #[test]
    fn days_outside_the_month_are_drawn_dimmed() {
        // Every square, against the date it actually holds: the dimmed ink on
        // the days either side of the month and nothing else, today's ink on
        // today wherever that falls.
        let p = palette();
        let today = day(2026, 8, 29);
        let mut spilled = 0;
        for month in 1..=12 {
            let m = Month { year: 2026, month };
            for start in [WeekStart::Sunday, WeekStart::Monday] {
                let rows = weeks(m, start, Some(today));
                let drawn = block(m, start, today, true, &p);
                for (i, row) in rows.iter().enumerate() {
                    // Row 0 is the heading, row 1 the weekday names, and the
                    // first cell of a grid row is the week-number gutter.
                    let cells = &drawn[2 + i];
                    for d in 0..7i64 {
                        let date = row.checked_add_signed(Duration::days(d)).expect("a date");
                        let (colour, text) = &cells[1 + d as usize];
                        assert_eq!(text.trim(), date.day().to_string());
                        let outside = date.month() != m.month || date.year() != m.year;
                        if date == today {
                            assert!(colour.contains(&p.today), "{} is today", date);
                        } else if outside {
                            spilled += 1;
                            assert!(colour.contains(&p.spill), "{} is not dimmed", date);
                        } else {
                            assert!(!colour.contains(&p.spill), "{} was dimmed", date);
                        }
                    }
                }
            }
        }
        assert!(spilled > 100, "only {} spill days to check", spilled);
    }

    /// A drawn row with its colours taken off, which is what a reader sees.
    fn plain(row: &str) -> String {
        let mut out = String::new();
        let mut chars = row.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for next in chars.by_ref() {
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    /// A drawn row's width in cells, escapes not counted.
    fn cells(row: &str) -> usize {
        tc::display_width(&plain(row))
    }

    /// Every word on the frame, however the rows fell.
    fn words(rows: &[String]) -> String {
        rows.iter()
            .flat_map(|row| {
                plain(row)
                    .split_whitespace()
                    .map(|w| w.to_string())
                    .collect::<Vec<String>>()
            })
            .collect::<Vec<String>>()
            .join(" ")
    }

    #[test]
    fn no_row_of_the_frame_overflows_the_pane() {
        // A row wider than the pane is not a truncation, it is worse: the
        // terminal wraps it, the frame's own row count is then wrong, and the
        // pinned title scrolls off the top. That is what a 26-column pane did
        // the first time this was drawn, and the reckoning line is exactly
        // where it happens, being the one row built from prose.
        let p = palette();
        let today = day(2026, 8, 29);
        let view = Month::of(today);
        // From 12, which is the narrowest the shared title fits in: below
        // that `tc::title` is already wider than the pane for every widget in
        // the tree, and that belongs to core rather than here. 400 is wider
        // than any wall. A long zone name and the note about a bad one are
        // the longest words the frame can carry.
        for w in 12..400usize {
            for note in [None, Some("timezone \"Antarctica/DumontDUrville\" is not in the database - reckoned on this machine's zone instead")] {
                let mut rows = frame(w, today, view, WeekStart::Sunday, "America/Argentina/ComodRivadavia (UTC-3)", note, true, true, &p);
                // The footer as it is drawn, not as it is packed: the margin
                // is part of the row, and leaving it out of the measurement
                // is how a footer one cell too wide goes unnoticed.
                rows.extend(footer(w, true, &p));
                // Arbitrary text in other widgets reaches the same shared
                // clipper. This fits by character count but not by terminal
                // columns: the final glyph occupies two. If seg lets it
                // through, the terminal wraps this row and moves the title.
                rows.push(tc::seg(
                    &[("", format!("{}界", "x".repeat(w.saturating_sub(1))))],
                    w,
                ));
                for row in &rows {
                    assert!(
                        cells(row) <= w,
                        "width {} drew a row of {}: {:?}",
                        w,
                        cells(row),
                        row
                    );
                }
            }
        }
    }

    #[test]
    fn the_reckoning_is_wrapped_rather_than_cut() {
        // Which zone, which week start and what the gutter counts are the
        // three things on screen that say how to read the grid, so none of
        // them may be lost on a narrow pane.
        let p = palette();
        let today = day(2026, 8, 29);
        for w in [26usize, 30, 40, 70] {
            let rows = frame(w, today, Month::of(today), WeekStart::Sunday, "Asia/Tokyo (UTC+9)", None, true, true, &p);
            let text = words(&rows);
            for word in ["2026-08-29", "SATURDAY", "Asia/Tokyo", "(UTC+9)", "Sunday"] {
                assert!(text.contains(word), "width {} lost {:?}", w, word);
            }
            // The ISO label appears exactly when there are numbers to label.
            let numbered = layout(w, true).is_some_and(|l| l.gutter);
            assert_eq!(
                text.contains("ISO 8601"),
                numbered,
                "width {}: gutter {} and the label disagree",
                w,
                numbered
            );
        }
    }

    fn luminance(c: (f64, f64, f64)) -> f64 {
        let ch = |x: f64| {
            let x = x / 255.0;
            if x <= 0.04045 {
                x / 12.92
            } else {
                ((x + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * ch(c.0) + 0.7152 * ch(c.1) + 0.0722 * ch(c.2)
    }

    fn contrast(a: (f64, f64, f64), b: (f64, f64, f64)) -> f64 {
        let (x, y) = (luminance(a), luminance(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    #[test]
    fn the_dimmed_days_clear_aa_against_the_backdrop_as_well_as_the_tint() {
        // check.rs measures every colour against the selected-row tint. The
        // backdrop is the other half of the convention and nothing reads it,
        // so the dimmed grey - the one colour this widget adds, and the one
        // the issue asks about by name - is measured here against both.
        let colours = [
            ("today", (140.0, 255.0, 205.0)),
            ("txt", (225.0, 235.0, 245.0)),
            ("wknd", (240.0, 190.0, 120.0)),
            ("spill", (140.0, 170.0, 195.0)),
            ("lbl", (130.0, 165.0, 200.0)),
            ("dim", (127.0, 147.0, 172.0)),
        ];
        // A dark terminal, and the tint today's square is drawn on.
        for (against, name) in [((16.0, 20.0, 26.0), "backdrop"), ((28.0, 44.0, 62.0), "tint")] {
            for (label, colour) in colours {
                let ratio = contrast(colour, against);
                assert!(
                    ratio >= 4.5,
                    "{} on the {} measures {:.2}, under AA 4.5",
                    label,
                    name,
                    ratio
                );
            }
        }
        // And dimmed has to actually look dimmer than a day of this month,
        // or the requirement is met by a colour nobody can tell apart.
        assert!(
            luminance((140.0, 170.0, 195.0)) < luminance((225.0, 235.0, 245.0)) * 0.6,
            "the spill is not visibly dimmer than the month"
        );
    }

    #[test]
    fn the_heading_names_the_month_and_the_year() {
        assert_eq!(Month { year: 2026, month: 8 }.heading(), "AUGUST 2026");
        assert_eq!(Month { year: 2026, month: 9 }.heading(), "SEPTEMBER 2026");
        // The longest heading still fits the narrowest block it is drawn in.
        for month in 1..=12 {
            let heading = Month { year: 2026, month }.heading();
            assert!(
                heading.chars().count() <= NARROWEST,
                "{:?} does not fit {} columns",
                heading,
                NARROWEST
            );
        }
    }
}
