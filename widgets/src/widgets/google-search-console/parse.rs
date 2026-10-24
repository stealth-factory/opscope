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

//! Search Console's answers, read into values, and the arithmetic done on
//! them.
//!
//! Nothing here touches the network, the clock or the disk: every function
//! takes text or values and returns values, so all of it compiles and is
//! tested on every target. The clock comes in as an argument wherever an
//! answer depends on what time it is.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc};
use chrono_tz::America::Los_Angeles;

/// Google's daily URL Inspection allowance, per property.
pub const DAILY_INSPECTIONS: usize = 2000;
/// How long an inspection is kept on disk before it is forgotten.
pub const KEEP_INSPECTIONS_DAYS: f64 = 14.0;
/// How long an inspection counts as today's, so a page is not asked about
/// twice inside a day unless somebody presses `i`.
pub const REINSPECT_AFTER: f64 = 24.0 * 3600.0;
/// The movers' row limit. Page and query are asked separately so neither
/// call groups both, which Google says raises the cost.
pub const MOVER_ROWS: usize = 1000;
/// The top search terms' row limit, kept small because page+query is the
/// expensive grouping.
pub const TERM_ROWS: usize = 25;

/// Clicks, impressions, CTR and average position, as Google reports them.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Metrics {
    pub clicks: f64,
    pub impressions: f64,
    pub ctr: f64,
    pub position: f64,
}

/// One row of a searchAnalytics answer.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub keys: Vec<String>,
    pub m: Metrics,
}

/// A whole searchAnalytics answer.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Analytics {
    pub rows: Vec<Row>,
    /// `metadata.first_incomplete_date`, only sent for `dataState: all`
    /// grouped by date when the range holds data still being collected.
    pub first_incomplete_date: Option<String>,
    /// `metadata.first_incomplete_hour`, the same for `hourly_all`.
    pub first_incomplete_hour: Option<String>,
}

fn num(v: &serde_json::Value) -> f64 {
    // Google sends counts as numbers here and as strings in sitemaps, so
    // both are read rather than trusting one shape.
    match v {
        serde_json::Value::Number(n) => n.as_f64().unwrap_or(0.0),
        serde_json::Value::String(s) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

fn string_at(v: &serde_json::Value, key: &str) -> String {
    v.get(key).and_then(|x| x.as_str()).unwrap_or("").to_string()
}

fn opt_text(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).filter(|s| !s.is_empty()).map(str::to_string)
}

fn json(text: &str) -> Result<serde_json::Value, String> {
    serde_json::from_str(text).map_err(|e| format!("Google answered with something that is not JSON ({e})"))
}

/// `searchAnalytics/query`. An answer with no `rows` is a real empty
/// answer - Google leaves the key out when nothing matched - not an error.
pub fn parse_search_analytics(text: &str) -> Result<Analytics, String> {
    let v = json(text)?;
    if !v.is_object() {
        return Err("Google's search analytics answer is not an object".into());
    }
    let rows = v
        .get("rows")
        .and_then(|r| r.as_array())
        .map(|rows| {
            rows.iter()
                .map(|r| Row {
                    keys: r
                        .get("keys")
                        .and_then(|k| k.as_array())
                        .map(|k| k.iter().filter_map(|x| x.as_str()).map(str::to_string).collect())
                        .unwrap_or_default(),
                    m: Metrics {
                        clicks: num(&r["clicks"]),
                        impressions: num(&r["impressions"]),
                        ctr: num(&r["ctr"]),
                        position: num(&r["position"]),
                    },
                })
                .collect()
        })
        .unwrap_or_default();
    let meta = &v["metadata"];
    Ok(Analytics {
        rows,
        first_incomplete_date: opt_text(meta, "first_incomplete_date"),
        first_incomplete_hour: opt_text(meta, "first_incomplete_hour"),
    })
}

/// One property the account can see, with its permission level.
#[derive(Clone, Debug, PartialEq)]
pub struct Site {
    pub url: String,
    pub permission: String,
}

/// `sites.list`. No `siteEntry` at all is an account that can see nothing.
pub fn parse_sites(text: &str) -> Result<Vec<Site>, String> {
    let v = json(text)?;
    Ok(v.get("siteEntry")
        .and_then(|s| s.as_array())
        .map(|s| {
            s.iter()
                .map(|e| Site { url: string_at(e, "siteUrl"), permission: string_at(e, "permissionLevel") })
                .filter(|s| !s.url.is_empty())
                .collect()
        })
        .unwrap_or_default())
}



/// What a sitemap holds of one type.
#[derive(Clone, Debug, PartialEq)]
pub struct Contents {
    pub kind: String,
    pub submitted: Option<i64>,
}

/// One sitemap, as `sitemaps.list` describes it.
#[derive(Clone, Debug, PartialEq)]
pub struct Sitemap {
    pub path: String,
    pub last_submitted: Option<String>,
    pub last_downloaded: Option<String>,
    pub pending: bool,
    pub index: bool,
    pub errors: i64,
    pub warnings: i64,
    pub contents: Vec<Contents>,
}

impl Sitemap {
    /// URLs submitted across every content type, or `None` when Google
    /// gave no count at all - which is not the same as zero.
    pub fn urls(&self) -> Option<i64> {
        let counted: Vec<i64> = self.contents.iter().filter_map(|c| c.submitted).collect();
        (!counted.is_empty()).then(|| counted.iter().sum())
    }
}

/// `sitemaps.list`, for a site or for the children of a sitemap index.
pub fn parse_sitemaps(text: &str) -> Result<Vec<Sitemap>, String> {
    let v = json(text)?;
    Ok(v.get("sitemap")
        .and_then(|s| s.as_array())
        .map(|s| {
            s.iter()
                .map(|m| Sitemap {
                    path: string_at(m, "path"),
                    last_submitted: opt_text(m, "lastSubmitted"),
                    last_downloaded: opt_text(m, "lastDownloaded"),
                    pending: m.get("isPending").and_then(|p| p.as_bool()).unwrap_or(false),
                    index: m.get("isSitemapsIndex").and_then(|p| p.as_bool()).unwrap_or(false),
                    errors: num(&m["errors"]) as i64,
                    warnings: num(&m["warnings"]) as i64,
                    contents: m
                        .get("contents")
                        .and_then(|c| c.as_array())
                        .map(|c| {
                            c.iter()
                                .map(|c| Contents {
                                    kind: string_at(c, "type"),
                                    submitted: c.get("submitted").map(|n| num(n) as i64),
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                })
                .filter(|m| !m.path.is_empty())
                .collect()
        })
        .unwrap_or_default())
}

/// One rich-result problem Google found on an inspected page.
#[derive(Clone, Debug, PartialEq)]
pub struct RichIssue {
    pub kind: String,
    pub message: String,
    pub severity: String,
}

/// What URL Inspection says about one page.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Inspection {
    pub verdict: String,
    pub coverage: String,
    pub robots: String,
    pub indexing: String,
    pub fetch: String,
    pub last_crawl: Option<String>,
    pub google_canonical: String,
    pub user_canonical: String,
    pub crawled_as: String,
    pub sitemaps: Vec<String>,
    pub referring: Vec<String>,
    pub link: String,
    pub rich_verdict: String,
    pub rich: Vec<RichIssue>,
}

impl Inspection {
    /// Google chose a canonical other than the one the page declares.
    pub fn canonical_differs(&self) -> bool {
        !self.google_canonical.is_empty()
            && !self.user_canonical.is_empty()
            && self.google_canonical != self.user_canonical
    }
}

fn strings(v: &serde_json::Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(|s| s.as_array())
        .map(|s| s.iter().filter_map(|x| x.as_str()).map(str::to_string).collect())
        .unwrap_or_default()
}

/// `urlInspection/index:inspect`.
pub fn parse_inspection(text: &str) -> Result<Inspection, String> {
    let v = json(text)?;
    let result = v
        .get("inspectionResult")
        .ok_or("Google's inspection answer has no inspectionResult")?;
    let idx = &result["indexStatusResult"];
    let rich = &result["richResultsResult"];
    let mut issues = Vec::new();
    for detected in rich.get("detectedItems").and_then(|d| d.as_array()).into_iter().flatten() {
        let kind = string_at(detected, "richResultType");
        for item in detected.get("items").and_then(|i| i.as_array()).into_iter().flatten() {
            for issue in item.get("issues").and_then(|i| i.as_array()).into_iter().flatten() {
                issues.push(RichIssue {
                    kind: kind.clone(),
                    message: string_at(issue, "issueMessage"),
                    severity: string_at(issue, "severity"),
                });
            }
        }
    }
    Ok(Inspection {
        verdict: string_at(idx, "verdict"),
        coverage: string_at(idx, "coverageState"),
        robots: string_at(idx, "robotsTxtState"),
        indexing: string_at(idx, "indexingState"),
        fetch: string_at(idx, "pageFetchState"),
        last_crawl: opt_text(idx, "lastCrawlTime"),
        google_canonical: string_at(idx, "googleCanonical"),
        user_canonical: string_at(idx, "userCanonical"),
        crawled_as: string_at(idx, "crawledAs"),
        sitemaps: strings(idx, "sitemap"),
        referring: strings(idx, "referringUrls"),
        link: string_at(result, "inspectionResultLink"),
        rich_verdict: string_at(rich, "verdict"),
        rich: issues,
    })
}

/// An RFC 3339 time as Unix seconds.
pub fn parse_rfc3339(text: &str) -> Option<f64> {
    DateTime::parse_from_rfc3339(text.trim())
        .ok()
        .map(|t| t.timestamp() as f64 + f64::from(t.timestamp_subsec_millis()) / 1000.0)
}

/// A `YYYY-MM-DD` date.
pub fn parse_date(text: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(text.trim(), "%Y-%m-%d").ok()
}

// ---------------------------------------------------------------- hours --

/// Whether Google counts an hour as done.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HourState {
    /// Before `first_incomplete_hour`: the figures are final for the hour.
    Complete,
    /// At or after `first_incomplete_hour`: Google is still collecting.
    Collecting,
    /// No marker came back, and the hour is later than any Google
    /// reported - so nothing is known about it yet.
    Unreported,
}

/// One hour of the LAST 24 HOURS strip.
#[derive(Clone, Debug, PartialEq)]
pub struct Hour {
    /// The hour's start, in UTC. Drawn in local time.
    pub start: DateTime<Utc>,
    pub state: HourState,
    /// `None` when Google sent no row for the hour. For a complete hour
    /// that is a real zero; for any other it is simply not known yet.
    pub m: Option<Metrics>,
}

impl Hour {
    pub fn clicks(&self) -> f64 {
        self.m.map(|m| m.clicks).unwrap_or(0.0)
    }
    pub fn impressions(&self) -> f64 {
        self.m.map(|m| m.impressions).unwrap_or(0.0)
    }
}

/// Totals for a set of hours, position weighted by impressions the way
/// Search Console itself averages it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Totals {
    pub hours: usize,
    pub clicks: f64,
    pub impressions: f64,
    /// `None` when there were no impressions to average a position over.
    pub position: Option<f64>,
}

fn add(t: &mut Totals, m: Option<&Metrics>) {
    t.hours += 1;
    if let Some(m) = m {
        t.clicks += m.clicks;
        t.impressions += m.impressions;
        if m.impressions > 0.0 {
            let weight = t.position.unwrap_or(0.0) * (t.impressions - m.impressions);
            t.position = Some((weight + m.position * m.impressions) / t.impressions);
        }
    }
}

/// The last 24 hours, and the same complete hours a week earlier.
#[derive(Clone, Debug, PartialEq)]
pub struct HourView {
    pub hours: Vec<Hour>,
    /// Whether Google returned any hourly row at all for the lookback. A
    /// new property gets none for days, which is not "0 clicks".
    pub any_rows: bool,
    /// The first and last hour Google returned - the lookback it actually
    /// gave, which the site detail states.
    pub lookback: Option<(DateTime<Utc>, DateTime<Utc>)>,
    /// The latest complete hour, for the title.
    pub latest_complete: Option<DateTime<Utc>>,
    pub now: Totals,
    /// The same hours seven days earlier, or `None` when the lookback
    /// does not reach back that far for every one of them, or a clock
    /// change leaves some hour without exactly one counterpart.
    pub week_ago: Option<Totals>,
    /// Why `week_ago` is `None` when that is a Pacific clock change rather
    /// than the lookback.
    pub clock_change: bool,
}

/// The start of the same Pacific wall-clock hour seven calendar days
/// before `at`, as Unix seconds, or `None` when that hour did not exist
/// (the spring-forward gap). Of a repeated hour (fall-back) the first is
/// taken.
pub fn week_earlier(at: DateTime<Utc>) -> Option<i64> {
    let local = at.with_timezone(&Los_Angeles).naive_local() - Duration::days(7);
    Los_Angeles.from_local_datetime(&local).earliest().map(|t| t.timestamp())
}

/// Build the strip from an `hour` + `hourly_all` answer.
///
/// `now` is passed in so the strip is a function of its inputs. The
/// window is the 24 hours ending with the one `now` falls in. Only
/// complete hours are compared, and each against the same hour seven days
/// earlier from the same answer - so both sides went through the same
/// collection and a still-collecting hour never reads as a drop.
pub fn hour_view(a: &Analytics, now: DateTime<Utc>) -> HourView {
    let mut by_hour: BTreeMap<i64, Metrics> = BTreeMap::new();
    for r in &a.rows {
        if let Some(at) = r.keys.first().and_then(|k| DateTime::parse_from_rfc3339(k).ok()) {
            by_hour.insert(at.timestamp(), r.m);
        }
    }
    let marker = a
        .first_incomplete_hour
        .as_deref()
        .and_then(|h| DateTime::parse_from_rfc3339(h).ok())
        .map(|t| t.timestamp());
    let latest_row = by_hour.keys().next_back().copied();
    let earliest_row = by_hour.keys().next().copied();
    let this_hour = now.timestamp() - now.timestamp().rem_euclid(3600);
    let mut hours = Vec::new();
    for back in (0..24).rev() {
        let at = this_hour - back * 3600;
        let state = match marker {
            Some(first) if at >= first => HourState::Collecting,
            Some(_) => HourState::Complete,
            // No marker: Google says that only when nothing in the range is
            // incomplete, so an hour it reported is complete. One past its
            // last row is an hour it has said nothing about.
            None => match latest_row {
                Some(last) if at <= last => HourState::Complete,
                _ => HourState::Unreported,
            },
        };
        hours.push(Hour {
            start: DateTime::<Utc>::from_timestamp(at, 0).unwrap_or(now),
            state,
            m: by_hour.get(&at).copied(),
        });
    }
    let mut totals = Totals::default();
    let mut prior = Totals::default();
    let mut reaches = true;
    let mut paired: HashSet<i64> = HashSet::new();
    let mut clock_change = false;
    for h in hours.iter().filter(|h| h.state == HourState::Complete) {
        add(&mut totals, h.m.as_ref());
        // The same Pacific wall-clock hour a calendar week earlier, which
        // is what Google's hourly keys are in. Across a clock change that
        // is not exactly 7 * 86400 seconds back; an hour with no single
        // counterpart, or two hours landing on one, leave nothing like for
        // like to compare, so the week is not compared at all.
        match week_earlier(h.start) {
            Some(then) if paired.insert(then) => {
                if earliest_row.is_none_or(|first| then < first) {
                    reaches = false;
                }
                add(&mut prior, by_hour.get(&then));
            }
            _ => clock_change = true,
        }
    }
    let latest_complete = hours
        .iter()
        .rev()
        .find(|h| h.state == HourState::Complete)
        .map(|h| h.start)
        .or_else(|| {
            // The strip may be all collecting while Google holds complete
            // hours further back; the title still names the latest one.
            let cut = marker.or(latest_row.map(|l| l + 3600))?;
            by_hour.keys().rev().find(|&&k| k < cut).and_then(|&k| DateTime::<Utc>::from_timestamp(k, 0))
        });
    HourView {
        any_rows: !by_hour.is_empty(),
        lookback: match (earliest_row, latest_row) {
            (Some(a), Some(b)) => DateTime::<Utc>::from_timestamp(a, 0).zip(DateTime::<Utc>::from_timestamp(b, 0)),
            _ => None,
        },
        latest_complete,
        week_ago: (totals.hours > 0 && reaches && !clock_change).then_some(prior),
        clock_change,
        now: totals,
        hours,
    }
}

// --------------------------------------------------------------- windows --

/// The last day each data state covers, read from a `date` grouping asked
/// with `dataState: all`.
///
/// Final ends the day before `first_incomplete_date` - that day itself,
/// not the last row before the marker, since a final day with no traffic
/// has no row; with no marker every day returned is final. Fresh ends at the last day with any data. `None`
/// means there is no such day: a brand-new property with nothing final yet.
pub fn last_days(a: &Analytics) -> (Option<NaiveDate>, Option<NaiveDate>) {
    let days: Vec<NaiveDate> = a.rows.iter().filter_map(|r| r.keys.first().and_then(|k| parse_date(k))).collect();
    let fresh = days.iter().max().copied();
    let final_end = match a.first_incomplete_date.as_deref().and_then(parse_date) {
        // Some row before the marker says the property had final days at
        // all; a brand-new one has none, whatever the marker says.
        Some(first) => days.iter().any(|d| *d < first).then(|| first - Duration::days(1)),
        None => fresh,
    };
    (final_end, fresh)
}

/// The property's daily totals from the same `date` grouping, oldest first.
pub fn daily(a: &Analytics) -> Vec<(NaiveDate, Metrics)> {
    let mut out: Vec<(NaiveDate, Metrics)> =
        a.rows.iter().filter_map(|r| r.keys.first().and_then(|k| parse_date(k)).map(|d| (d, r.m))).collect();
    out.sort_by_key(|(d, _)| *d);
    out
}

/// Totals over the days of one window. These are the property's own
/// totals, aggregated by Google, not a sum of the rows a page or query
/// grouping happened to return.
pub fn window_totals(days: &[(NaiveDate, Metrics)], (from, to): (NaiveDate, NaiveDate)) -> Totals {
    let mut t = Totals::default();
    for (_, m) in days.iter().filter(|(d, _)| *d >= from && *d <= to) {
        add(&mut t, Some(m));
    }
    t
}

/// The current window of `days` complete days ending at `end`, and the
/// equal window before it.
pub fn windows(end: NaiveDate, days: i64) -> ((NaiveDate, NaiveDate), (NaiveDate, NaiveDate)) {
    let days = days.max(1);
    let cur_start = end - Duration::days(days - 1);
    let prior_end = cur_start - Duration::days(1);
    ((cur_start, end), (prior_end - Duration::days(days - 1), prior_end))
}

/// `Oct 12–18`, or `Sep 28–Oct 4` across a month.
pub fn span_label(from: NaiveDate, to: NaiveDate) -> String {
    if from.format("%b").to_string() == to.format("%b").to_string() {
        format!("{}–{}", from.format("%b %-d"), to.format("%-d"))
    } else {
        format!("{}–{}", from.format("%b %-d"), to.format("%b %-d"))
    }
}

// ---------------------------------------------------------------- movers --

/// What the movers are sorted by. `m` steps through them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Metric {
    Impressions,
    Clicks,
    Ctr,
    Position,
}

pub const METRICS: [Metric; 4] = [Metric::Impressions, Metric::Clicks, Metric::Ctr, Metric::Position];

impl Metric {
    pub fn name(self) -> &'static str {
        match self {
            Metric::Impressions => "impressions",
            Metric::Clicks => "clicks",
            Metric::Ctr => "CTR",
            Metric::Position => "position",
        }
    }
    pub fn of(self, m: &Metrics) -> f64 {
        match self {
            Metric::Impressions => m.impressions,
            Metric::Clicks => m.clicks,
            Metric::Ctr => m.ctr,
            Metric::Position => m.position,
        }
    }
}

/// One page or query, in both windows.
#[derive(Clone, Debug, PartialEq)]
pub struct Mover {
    pub key: String,
    pub cur: Option<Metrics>,
    pub prior: Option<Metrics>,
    /// The missing side is absent only from an answer Google's row limit
    /// cut, so it is not known to be zero: the row may sit just below the
    /// top `MOVER_ROWS` of that window. Such a row is neither new nor gone.
    pub unknown: bool,
}

impl Mover {
    /// In the current window and known to be absent from the prior one.
    pub fn is_new(&self) -> bool {
        self.cur.is_some() && self.prior.is_none() && !self.unknown
    }
    /// In the prior window and known to be absent from the current one:
    /// it dropped to zero.
    pub fn is_gone(&self) -> bool {
        self.cur.is_none() && self.prior.is_some() && !self.unknown
    }

    /// How far it moved on `metric`, with improvement positive.
    ///
    /// A missing side counts as zero for clicks and impressions, which is
    /// what absence from Google's rows means for those. For CTR and
    /// position it means nothing - a page with no impressions has no
    /// position - so a new or gone row has no change there.
    pub fn change(&self, metric: Metric) -> Option<f64> {
        match (self.cur, self.prior, metric) {
            (Some(c), Some(p), Metric::Position) => Some(p.position - c.position),
            (Some(c), Some(p), _) => Some(metric.of(&c) - metric.of(&p)),
            (_, _, Metric::Ctr | Metric::Position) => None,
            // A side cut by the row limit is unknown, not zero.
            _ if self.unknown => None,
            (c, p, _) => Some(c.map(|m| metric.of(&m)).unwrap_or(0.0) - p.map(|m| metric.of(&m)).unwrap_or(0.0)),
        }
    }
}

/// Join a current and a prior answer by their first key.
///
/// An answer that came back with `MOVER_ROWS` rows was cut by Google's row
/// limit, so a key missing from it is marked unknown rather than zero.
pub fn join_movers(cur: &[Row], prior: &[Row]) -> Vec<Mover> {
    let (cur_cut, prior_cut) = (cur.len() >= MOVER_ROWS, prior.len() >= MOVER_ROWS);
    let mut out: Vec<Mover> = Vec::new();
    let mut at: HashMap<String, usize> = HashMap::new();
    for r in cur {
        let key = r.keys.first().cloned().unwrap_or_default();
        at.insert(key.clone(), out.len());
        out.push(Mover { key, cur: Some(r.m), prior: None, unknown: prior_cut });
    }
    for r in prior {
        let key = r.keys.first().cloned().unwrap_or_default();
        match at.get(&key) {
            Some(&i) => {
                out[i].prior = Some(r.m);
                out[i].unknown = false;
            }
            None => {
                at.insert(key.clone(), out.len());
                out.push(Mover { key, cur: None, prior: Some(r.m), unknown: cur_cut });
            }
        }
    }
    out
}

/// Biggest movement first, either direction. Rows with no measurable
/// change on this metric go last, ordered by impressions, rather than
/// being dropped - a new page is news whatever it is sorted by.
pub fn sort_movers(movers: &mut [Mover], metric: Metric) {
    let weight = |m: &Mover| m.cur.or(m.prior).map(|x| x.impressions).unwrap_or(0.0);
    movers.sort_by(|a, b| match (a.change(metric), b.change(metric)) {
        (Some(x), Some(y)) => y.abs().total_cmp(&x.abs()).then_with(|| weight(b).total_cmp(&weight(a))),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => weight(b).total_cmp(&weight(a)),
    });
}

/// Percent change from `prior` to `cur`, or `None` when there is nothing
/// to divide by.
pub fn percent(cur: f64, prior: f64) -> Option<f64> {
    (prior != 0.0).then(|| (cur - prior) / prior * 100.0)
}

// ----------------------------------------------------------------- terms --

/// One search term and the page it lands on most.
#[derive(Clone, Debug, PartialEq)]
pub struct Term {
    pub query: String,
    pub page: String,
    pub m: Metrics,
    /// Clicks for the same query and page in the prior window, or `None`
    /// when that pair was not in the prior window's top rows - which is
    /// not the same as zero, so it is not drawn as one.
    pub prior_clicks: Option<f64>,
    /// The other pages the same query lands on, from the same top rows.
    pub others: Vec<(String, Metrics)>,
}

/// Group `query, page` rows by query. The landing page is the pair with
/// the most clicks; Google's order (by clicks) is kept for the queries.
pub fn group_terms(cur: &[Row], prior: &[Row]) -> Vec<Term> {
    let mut prior_at: HashMap<(String, String), f64> = HashMap::new();
    for r in prior {
        if let [q, p, ..] = r.keys.as_slice() {
            prior_at.insert((q.clone(), p.clone()), r.m.clicks);
        }
    }
    let mut out: Vec<Term> = Vec::new();
    let mut at: HashMap<String, usize> = HashMap::new();
    for r in cur {
        let [q, p, ..] = r.keys.as_slice() else { continue };
        match at.get(q) {
            Some(&i) => {
                let t = &mut out[i];
                if r.m.clicks > t.m.clicks {
                    let was = (std::mem::replace(&mut t.page, p.clone()), t.m);
                    t.others.push(was);
                    t.m = r.m;
                    t.prior_clicks = prior_at.get(&(q.clone(), p.clone())).copied();
                } else {
                    t.others.push((p.clone(), r.m));
                }
            }
            None => {
                at.insert(q.clone(), out.len());
                out.push(Term {
                    query: q.clone(),
                    page: p.clone(),
                    m: r.m,
                    prior_clicks: prior_at.get(&(q.clone(), p.clone())).copied(),
                    others: Vec::new(),
                });
            }
        }
    }
    for t in &mut out {
        t.others.sort_by(|a, b| b.1.clicks.total_cmp(&a.1.clicks));
    }
    out
}

// ----------------------------------------------------------- inspections --

/// One page's inspection as kept on disk: when it was asked and what
/// Google said, or the refusal Google gave for that row.
#[derive(Clone, Debug, PartialEq)]
pub struct Inspected {
    pub at: f64,
    pub result: Result<Inspection, String>,
    /// The raw answer, kept so the cache survives a parser change.
    pub raw: String,
}

/// The inspection cache for one property.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InspectionCache {
    /// The Pacific date `used` counts for - Google's quota day.
    pub day: String,
    /// Inspections this widget spent on `day`.
    pub used: usize,
    pub pages: BTreeMap<String, Inspected>,
    /// Pages with a request out now, already counted in `used`. Never
    /// written to disk: it lives only as long as the requests do.
    pub inflight: BTreeSet<String>,
}

/// Read `<site>.inspections.json`. A missing or damaged file is an empty
/// cache: the worst it costs is asking again.
pub fn parse_inspection_cache(text: &str) -> InspectionCache {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return InspectionCache::default();
    };
    let mut pages = BTreeMap::new();
    if let Some(map) = v.get("pages").and_then(|p| p.as_object()) {
        for (url, e) in map {
            let at = e.get("at").and_then(|a| a.as_f64()).unwrap_or(0.0);
            let raw = e.get("raw").and_then(|r| r.as_str()).unwrap_or("").to_string();
            let result = match e.get("error").and_then(|r| r.as_str()) {
                Some(err) => Err(err.to_string()),
                None => parse_inspection(&raw),
            };
            pages.insert(url.clone(), Inspected { at, result, raw });
        }
    }
    InspectionCache {
        day: v.get("day").and_then(|d| d.as_str()).unwrap_or("").to_string(),
        used: v.get("used").and_then(|u| u.as_u64()).unwrap_or(0) as usize,
        pages,
        inflight: BTreeSet::new(),
    }
}

impl InspectionCache {
    /// The file form of the cache.
    pub fn to_json(&self) -> String {
        let mut pages = serde_json::Map::new();
        for (url, e) in &self.pages {
            let mut o = serde_json::json!({ "at": e.at, "raw": e.raw });
            if let Err(err) = &e.result {
                o["error"] = serde_json::Value::String(err.clone());
            }
            pages.insert(url.clone(), o);
        }
        serde_json::json!({ "day": self.day, "used": self.used, "pages": pages }).to_string()
    }

    /// Start a new quota day when Google's has turned over, and forget
    /// what is older than the cache keeps.
    pub fn roll(&mut self, today: &str, now: f64) {
        if self.day != today {
            self.day = today.to_string();
            self.used = 0;
        }
        self.pages.retain(|_, e| now - e.at < KEEP_INSPECTIONS_DAYS * 86400.0);
    }

    /// Inspections left today.
    pub fn left(&self) -> usize {
        DAILY_INSPECTIONS.saturating_sub(self.used)
    }

    /// Which of `wanted` to inspect now: those never asked about or asked
    /// about more than a day ago, in the order given, stopping where the
    /// day's budget does.
    pub fn due(&self, wanted: &[String], now: f64) -> Vec<String> {
        wanted
            .iter()
            .filter(|u| !self.inflight.contains(*u))
            .filter(|u| self.pages.get(*u).is_none_or(|e| now - e.at >= REINSPECT_AFTER))
            .take(self.left())
            .cloned()
            .collect()
    }

    /// Take one of today's inspections for `url` before asking Google, so
    /// two requests started together cannot both spend the last one, and
    /// a page already being asked about is not asked about again.
    pub fn reserve(&mut self, url: &str) -> Result<(), String> {
        if self.inflight.contains(url) {
            return Err("already being inspected".into());
        }
        if self.left() == 0 {
            return Err(format!("budget spent: today's {} inspections are used", DAILY_INSPECTIONS));
        }
        self.used += 1;
        self.inflight.insert(url.to_string());
        Ok(())
    }

    /// Give a reservation back: the request was never sent.
    pub fn release(&mut self, url: &str) {
        if self.inflight.remove(url) {
            self.used = self.used.saturating_sub(1);
        }
    }

    /// End a reservation whose request was sent but left nothing to keep:
    /// it stays counted, since Google counted it.
    pub fn abandon(&mut self, url: &str) {
        self.inflight.remove(url);
    }

    /// Record one inspection: it counts against the day whatever Google
    /// answered, since a refused inspection still spent the request - once,
    /// whether or not it was reserved first.
    pub fn record(&mut self, url: &str, at: f64, raw: String, result: Result<Inspection, String>) {
        if !self.inflight.remove(url) {
            self.used += 1;
        }
        self.pages.insert(url.to_string(), Inspected { at, result, raw });
    }
}

/// The pages worth inspecting: the top `top` by impressions in the
/// current window, then up to `top` that dropped to zero, by what they
/// had before. Returns the set and how many of each.
pub fn inspection_set(movers: &[Mover], top: usize) -> (Vec<String>, usize, usize) {
    let mut current: Vec<&Mover> = movers.iter().filter(|m| m.cur.is_some()).collect();
    current.sort_by(|a, b| {
        let (x, y) = (a.cur.map(|m| m.impressions).unwrap_or(0.0), b.cur.map(|m| m.impressions).unwrap_or(0.0));
        y.total_cmp(&x)
    });
    let mut gone: Vec<&Mover> = movers.iter().filter(|m| m.is_gone()).collect();
    gone.sort_by(|a, b| {
        let (x, y) = (a.prior.map(|m| m.impressions).unwrap_or(0.0), b.prior.map(|m| m.impressions).unwrap_or(0.0));
        y.total_cmp(&x)
    });
    let tops: Vec<String> = current.iter().take(top).map(|m| m.key.clone()).collect();
    let dropped: Vec<String> = if top == 0 { Vec::new() } else { gone.iter().take(top).map(|m| m.key.clone()).collect() };
    let (a, b) = (tops.len(), dropped.len());
    let mut set = tops;
    set.extend(dropped);
    (set, a, b)
}

/// A file name for a property: the name made safe to read, then a hash of
/// the whole name, since making it safe is not reversible -
/// `https://example.com/a/b/` and `https://example.com/a_b/` read alike and
/// must not share a cache or its count.
pub fn cache_name(site: &str) -> String {
    let safe: String = site
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' })
        .collect();
    // FNV-1a, spelled out: std's hasher may change between Rust releases,
    // and a file name has to stay the same across them.
    let hash = site.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3));
    format!("{}-{:016x}.inspections.json", safe, hash)
}

// ------------------------------------------------------------------ sites --

/// What is wrong with a configured site name, if its form is not one
/// Search Console uses.
pub fn site_form_problem(site: &str) -> Option<String> {
    let s = site.trim();
    if let Some(domain) = s.strip_prefix("sc-domain:") {
        return (domain.is_empty() || domain.contains('/')).then(|| {
            format!("{} is not a domain property name: write sc-domain:example.com, with no scheme or path", s)
        });
    }
    if s.starts_with("http://") || s.starts_with("https://") {
        return (!s.ends_with('/')).then(|| {
            format!("{} has no trailing slash, and Search Console names a URL-prefix property with one: {}/", s, s)
        });
    }
    Some(format!(
        "{} is neither form Search Console uses: sc-domain:{} for a domain property, or https://{}/ for a URL prefix",
        s,
        s.trim_end_matches('/'),
        s.trim_end_matches('/')
    ))
}

/// The host a property name is about, for telling somebody which of the
/// properties they can see is the one they meant.
pub fn site_host(site: &str) -> String {
    let s = site.trim();
    let s = s.strip_prefix("sc-domain:").unwrap_or(s);
    let s = s.trim_start_matches("https://").trim_start_matches("http://");
    let s = s.split('/').next().unwrap_or("");
    s.trim_start_matches("www.").to_lowercase()
}

/// A page URL as short as the property lets it be: the path for a page on
/// the property's own host, the URL without its scheme otherwise.
pub fn short_page(site: &str, url: &str) -> String {
    if let Some(prefix) = site.strip_suffix('/').filter(|_| site.starts_with("http")) {
        if let Some(rest) = url.strip_prefix(prefix) {
            return if rest.is_empty() { "/".into() } else { rest.to_string() };
        }
    }
    if let Some(domain) = site.strip_prefix("sc-domain:") {
        for scheme in ["https://", "http://"] {
            for host in [domain.to_string(), format!("www.{}", domain)] {
                if let Some(rest) = url.strip_prefix(&format!("{}{}", scheme, host)) {
                    if rest.is_empty() || rest.starts_with('/') {
                        return if rest.is_empty() { "/".into() } else { rest.to_string() };
                    }
                }
            }
        }
    }
    url.trim_start_matches("https://").trim_start_matches("http://").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn utc(y: i32, mo: u32, d: u32, h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, 0, 0).unwrap()
    }

    /// Hourly rows in Pacific time, as Google sends them, for every hour
    /// from `from` to `to` (UTC), with clicks equal to the hour of day.
    fn hourly(from: DateTime<Utc>, to: DateTime<Utc>, skip: &[DateTime<Utc>], marker: Option<&str>) -> String {
        let mut rows = Vec::new();
        let mut t = from;
        while t <= to {
            if !skip.contains(&t) {
                let pacific = t.with_timezone(&chrono::FixedOffset::west_opt(7 * 3600).unwrap());
                rows.push(serde_json::json!({
                    "keys": [pacific.to_rfc3339()],
                    "clicks": 10, "impressions": 100, "ctr": 0.1, "position": 8.0
                }));
            }
            t += Duration::hours(1);
        }
        let mut v = serde_json::json!({ "rows": rows, "responseAggregationType": "byProperty" });
        if let Some(m) = marker {
            v["metadata"] = serde_json::json!({ "first_incomplete_hour": m });
        }
        v.to_string()
    }

    #[test]
    fn search_analytics_reads_rows_and_both_markers() {
        let a = parse_search_analytics(
            r#"{"rows":[{"keys":["/pricing"],"clicks":3120,"impressions":41200,"ctr":0.0757,"position":4.1}],
               "metadata":{"first_incomplete_date":"2026-10-19","first_incomplete_hour":"2026-10-21T09:00:00-07:00"}}"#,
        )
        .unwrap();
        assert_eq!(a.rows.len(), 1);
        assert_eq!(a.rows[0].keys, vec!["/pricing"]);
        assert_eq!(a.rows[0].m.clicks, 3120.0);
        assert_eq!(a.first_incomplete_date.as_deref(), Some("2026-10-19"));
        assert_eq!(a.first_incomplete_hour.as_deref(), Some("2026-10-21T09:00:00-07:00"));
    }

    #[test]
    fn an_answer_with_no_rows_is_empty_not_an_error() {
        let a = parse_search_analytics(r#"{"responseAggregationType":"byPage"}"#).unwrap();
        assert!(a.rows.is_empty());
        assert!(parse_search_analytics("<html>").is_err());
    }

    #[test]
    fn query_and_page_rows_keep_both_keys() {
        let a = parse_search_analytics(
            r#"{"rows":[{"keys":["example widget","https://example.com/pricing"],"clicks":40,"impressions":900,"ctr":0.044,"position":3.2}]}"#,
        )
        .unwrap();
        assert_eq!(a.rows[0].keys, vec!["example widget", "https://example.com/pricing"]);
    }

    #[test]
    fn sites_list_reads_url_and_permission() {
        let s = parse_sites(
            r#"{"siteEntry":[{"siteUrl":"sc-domain:example.com","permissionLevel":"siteRestrictedUser"},
                             {"siteUrl":"https://example.org/","permissionLevel":"siteUnverifiedUser"}]}"#,
        )
        .unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].url, "sc-domain:example.com");
        assert_eq!(s[1].permission, "siteUnverifiedUser");
        assert!(parse_sites("{}").unwrap().is_empty());
    }

    #[test]
    fn sitemaps_read_counts_sent_as_strings() {
        let s = parse_sitemaps(
            r#"{"sitemap":[{"path":"https://example.com/sitemap.xml","lastSubmitted":"2026-10-01T00:00:00Z",
                "isPending":false,"isSitemapsIndex":true,"lastDownloaded":"2026-10-20T03:00:00.000Z",
                "warnings":"3","errors":"0","contents":[{"type":"web","submitted":"1214","indexed":"0"},
                {"type":"image","submitted":"10"}]}]}"#,
        )
        .unwrap();
        assert_eq!(s[0].warnings, 3);
        assert_eq!(s[0].errors, 0);
        assert!(s[0].index);
        assert_eq!(s[0].urls(), Some(1224));
        assert_eq!(s[0].last_downloaded.as_deref(), Some("2026-10-20T03:00:00.000Z"));
    }

    #[test]
    fn a_sitemap_with_no_counts_has_no_url_total() {
        let s = parse_sitemaps(r#"{"sitemap":[{"path":"https://example.com/a.xml","isPending":true}]}"#).unwrap();
        assert_eq!(s[0].urls(), None);
        assert!(s[0].pending);
        assert_eq!(s[0].last_downloaded, None);
    }

    const INSPECTED: &str = r#"{"inspectionResult":{
        "inspectionResultLink":"https://search.google.com/search-console/inspect?resource_id=sc-domain:example.com&id=x",
        "indexStatusResult":{"verdict":"FAIL","coverageState":"Submitted and indexed","robotsTxtState":"ALLOWED",
          "indexingState":"INDEXING_ALLOWED","lastCrawlTime":"2026-10-20T08:00:00Z","pageFetchState":"SOFT_404",
          "googleCanonical":"https://example.com/old","userCanonical":"https://example.com/new",
          "sitemap":["https://example.com/sitemap.xml"],"referringUrls":["https://example.com/"],"crawledAs":"MOBILE"},
        "richResultsResult":{"verdict":"PARTIAL","detectedItems":[{"richResultType":"Breadcrumbs","items":[{"name":"x",
          "issues":[{"issueMessage":"Missing field \"item\"","severity":"WARNING"}]}]}]}}}"#;

    #[test]
    fn inspection_reads_every_field_the_section_names() {
        let i = parse_inspection(INSPECTED).unwrap();
        assert_eq!(i.verdict, "FAIL");
        assert_eq!(i.fetch, "SOFT_404");
        assert_eq!(i.indexing, "INDEXING_ALLOWED");
        assert!(i.canonical_differs());
        assert_eq!(i.sitemaps.len(), 1);
        assert_eq!(i.referring.len(), 1);
        assert_eq!(i.rich.len(), 1);
        assert_eq!(i.rich[0].kind, "Breadcrumbs");
        assert!(i.link.starts_with("https://search.google.com/"));
        assert!(parse_inspection("{}").is_err());
    }

    #[test]
    fn hours_from_the_marker_on_are_collecting_and_pacific_becomes_utc() {
        let now = utc(2026, 10, 21, 18) + Duration::minutes(20);
        // 09:00 Pacific daylight time is 16:00 UTC.
        let text = hourly(utc(2026, 10, 11, 0), utc(2026, 10, 21, 18), &[], Some("2026-10-21T09:00:00-07:00"));
        let v = hour_view(&parse_search_analytics(&text).unwrap(), now);
        assert_eq!(v.hours.len(), 24);
        assert_eq!(v.hours.last().unwrap().start, utc(2026, 10, 21, 18));
        let collecting: Vec<_> = v.hours.iter().filter(|h| h.state == HourState::Collecting).collect();
        assert_eq!(collecting.len(), 3, "16:00, 17:00 and 18:00 UTC");
        assert_eq!(collecting[0].start, utc(2026, 10, 21, 16));
        assert_eq!(v.latest_complete, Some(utc(2026, 10, 21, 15)));
        assert_eq!(v.now.hours, 21);
        assert_eq!(v.now.clicks, 210.0);
        let prior = v.week_ago.expect("ten days of lookback reaches a week back");
        assert_eq!(prior.clicks, 210.0);
        assert_eq!(prior.hours, 21);
        assert_eq!(v.now.position, Some(8.0));
    }

    #[test]
    fn a_complete_hour_with_no_row_is_zero_and_a_collecting_one_is_unknown() {
        let now = utc(2026, 10, 21, 18);
        let gap = utc(2026, 10, 21, 3);
        let late = utc(2026, 10, 21, 17);
        let text = hourly(utc(2026, 10, 11, 0), now, &[gap, late], Some("2026-10-21T09:00:00-07:00"));
        let v = hour_view(&parse_search_analytics(&text).unwrap(), now);
        let zero = v.hours.iter().find(|h| h.start == gap).unwrap();
        assert_eq!(zero.state, HourState::Complete);
        assert_eq!(zero.m, None);
        assert_eq!(zero.clicks(), 0.0);
        let unknown = v.hours.iter().find(|h| h.start == late).unwrap();
        assert_eq!(unknown.state, HourState::Collecting);
        // The zero hour counts in the total as zero, and its week-ago twin
        // still counts on the other side: like for like.
        assert_eq!(v.now.clicks, 200.0);
        assert_eq!(v.week_ago.unwrap().clicks, 210.0);
    }

    #[test]
    fn no_rows_at_all_is_said_differently_from_zero_clicks() {
        let v = hour_view(&parse_search_analytics("{}").unwrap(), utc(2026, 10, 21, 18));
        assert!(!v.any_rows);
        assert!(v.hours.iter().all(|h| h.state == HourState::Unreported));
        assert_eq!(v.week_ago, None);
        assert_eq!(v.latest_complete, None);
    }

    #[test]
    fn every_hour_collecting_compares_nothing() {
        let now = utc(2026, 10, 21, 18);
        let text = hourly(utc(2026, 10, 11, 0), now, &[], Some("2026-10-20T00:00:00-07:00"));
        let v = hour_view(&parse_search_analytics(&text).unwrap(), now);
        assert!(v.hours.iter().all(|h| h.state == HourState::Collecting));
        assert_eq!(v.now.hours, 0);
        assert_eq!(v.week_ago, None);
        // The title still has a complete hour to name, further back.
        assert_eq!(v.latest_complete, Some(utc(2026, 10, 20, 6)));
    }

    #[test]
    fn a_lookback_short_of_a_week_gives_no_comparison() {
        let now = utc(2026, 10, 21, 18);
        let text = hourly(utc(2026, 10, 18, 0), now, &[], Some("2026-10-21T09:00:00-07:00"));
        let v = hour_view(&parse_search_analytics(&text).unwrap(), now);
        assert!(v.now.hours > 0);
        assert_eq!(v.week_ago, None);
    }

    #[test]
    fn with_no_marker_hours_past_the_last_row_are_unreported() {
        let now = utc(2026, 10, 21, 18);
        let text = hourly(utc(2026, 10, 11, 0), utc(2026, 10, 21, 14), &[], None);
        let v = hour_view(&parse_search_analytics(&text).unwrap(), now);
        let unreported = v.hours.iter().filter(|h| h.state == HourState::Unreported).count();
        assert_eq!(unreported, 4);
        assert_eq!(v.latest_complete, Some(utc(2026, 10, 21, 14)));
    }

    fn dates(days: &[&str], marker: Option<&str>) -> Analytics {
        Analytics {
            rows: days
                .iter()
                .map(|d| Row { keys: vec![d.to_string()], m: Metrics::default() })
                .collect(),
            first_incomplete_date: marker.map(str::to_string),
            first_incomplete_hour: None,
        }
    }

    #[test]
    fn final_ends_the_day_before_the_first_incomplete_one() {
        let a = dates(&["2026-10-17", "2026-10-18", "2026-10-19", "2026-10-20"], Some("2026-10-19"));
        let (fin, fresh) = last_days(&a);
        assert_eq!(fin, parse_date("2026-10-18"));
        assert_eq!(fresh, parse_date("2026-10-20"));
    }

    #[test]
    fn a_final_day_with_no_traffic_still_ends_the_final_window() {
        // The 18th was final and had nothing, so Google sent no row for it.
        let a = dates(&["2026-10-16", "2026-10-17", "2026-10-20"], Some("2026-10-19"));
        assert_eq!(last_days(&a).0, parse_date("2026-10-18"));
    }

    #[test]
    fn a_week_back_is_the_same_pacific_hour_across_a_clock_change() {
        // 2026-11-01 is the fall-back Sunday in the US. Noon Pacific on the
        // 3rd (PST, 20:00 UTC) pairs with noon Pacific on Oct 27 (PDT,
        // 19:00 UTC): 169 hours back, not 168.
        let noon = Utc.with_ymd_and_hms(2026, 11, 3, 20, 0, 0).unwrap();
        assert_eq!(week_earlier(noon), Some(Utc.with_ymd_and_hms(2026, 10, 27, 19, 0, 0).unwrap().timestamp()));
        // An ordinary week is exactly 168 hours.
        let plain = Utc.with_ymd_and_hms(2026, 10, 21, 16, 0, 0).unwrap();
        assert_eq!(week_earlier(plain), Some(plain.timestamp() - 7 * 86400));
        // 02:00 on 2026-03-15 Pacific: a week earlier, 2026-03-08, skipped it.
        let gap = Utc.with_ymd_and_hms(2026, 3, 15, 9, 0, 0).unwrap();
        assert_eq!(week_earlier(gap), None);
    }

    #[test]
    fn a_window_cut_by_the_row_limit_calls_nothing_new_or_gone() {
        let full: Vec<Row> = (0..MOVER_ROWS).map(|i| row(&format!("/p{i}"), 1.0, 10.0, 5.0)).collect();
        let prior = vec![row("/p0", 1.0, 10.0, 5.0), row("/slipped", 2.0, 20.0, 5.0)];
        let m = join_movers(&full, &prior);
        let slipped = m.iter().find(|m| m.key == "/slipped").unwrap();
        assert!(!slipped.is_gone(), "absent from a cut answer is not zero");
        assert_eq!(slipped.change(Metric::Impressions), None);
        // The whole prior answer was not cut, so a page missing there is new.
        assert!(m.iter().find(|m| m.key == "/p1").unwrap().is_new());
        assert!(inspection_set(&m, 5).0.iter().all(|u| u != "/slipped"));
        // And the other way round.
        let m = join_movers(&prior, &full);
        let slipped = m.iter().find(|m| m.key == "/slipped").unwrap();
        assert!(!slipped.is_new());
        assert!(!m.iter().find(|m| m.key == "/p0").unwrap().unknown);
    }

    #[test]
    fn a_new_property_has_no_final_day() {
        let a = dates(&["2026-10-20"], Some("2026-10-20"));
        assert_eq!(last_days(&a), (None, parse_date("2026-10-20")));
        assert_eq!(last_days(&dates(&[], None)), (None, None));
    }

    #[test]
    fn windows_are_equal_and_adjacent() {
        let ((a, b), (c, d)) = windows(parse_date("2026-10-18").unwrap(), 7);
        assert_eq!((a, b), (parse_date("2026-10-12").unwrap(), parse_date("2026-10-18").unwrap()));
        assert_eq!((c, d), (parse_date("2026-10-05").unwrap(), parse_date("2026-10-11").unwrap()));
        assert_eq!(span_label(a, b), "Oct 12–18");
        assert_eq!(span_label(parse_date("2026-09-28").unwrap(), parse_date("2026-10-04").unwrap()), "Sep 28–Oct 4");
    }

    fn row(key: &str, clicks: f64, impressions: f64, position: f64) -> Row {
        Row {
            keys: vec![key.to_string()],
            m: Metrics { clicks, impressions, ctr: if impressions > 0.0 { clicks / impressions } else { 0.0 }, position },
        }
    }

    #[test]
    fn movers_join_and_sort_by_absolute_change_with_new_and_gone() {
        let cur = vec![row("/a", 10.0, 1000.0, 5.0), row("/b", 5.0, 200.0, 9.0), row("/new", 1.0, 700.0, 6.0)];
        let prior = vec![row("/a", 8.0, 400.0, 4.0), row("/b", 5.0, 210.0, 7.0), row("/gone", 3.0, 900.0, 3.0)];
        let mut m = join_movers(&cur, &prior);
        assert_eq!(m.len(), 4);
        sort_movers(&mut m, Metric::Impressions);
        let keys: Vec<&str> = m.iter().map(|m| m.key.as_str()).collect();
        assert_eq!(keys, vec!["/gone", "/new", "/a", "/b"]);
        assert!(m[0].is_gone() && m[1].is_new());
        assert_eq!(m[0].change(Metric::Impressions), Some(-900.0));
        // Position: lower is better, so a fall from 7 to 9 is -2.
        let b = m.iter().find(|m| m.key == "/b").unwrap();
        assert_eq!(b.change(Metric::Position), Some(-2.0));
        // A new page has no position change, and sorts after those that do.
        sort_movers(&mut m, Metric::Position);
        assert_eq!(m[0].key, "/b");
        assert!(m[2].change(Metric::Position).is_none());
        assert_eq!(percent(1000.0, 400.0), Some(150.0));
        assert_eq!(percent(5.0, 0.0), None);
    }

    #[test]
    fn terms_group_pages_under_their_query() {
        let pair = |q: &str, p: &str, clicks: f64| Row {
            keys: vec![q.into(), p.into()],
            m: Metrics { clicks, impressions: clicks * 10.0, ctr: 0.1, position: 3.0 },
        };
        let cur = vec![
            pair("example widget", "https://example.com/a", 40.0),
            pair("example tool", "https://example.com/b", 30.0),
            pair("example widget", "https://example.com/c", 50.0),
            pair("example widget", "https://example.com/d", 2.0),
        ];
        let prior = vec![pair("example widget", "https://example.com/c", 45.0)];
        let t = group_terms(&cur, &prior);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].query, "example widget");
        assert_eq!(t[0].page, "https://example.com/c", "the page with the most clicks lands it");
        assert_eq!(t[0].prior_clicks, Some(45.0));
        assert_eq!(t[0].others.len(), 2);
        assert_eq!(t[0].others[0].0, "https://example.com/a");
        assert_eq!(t[1].prior_clicks, None, "absent from the prior top rows is not zero");
    }

    #[test]
    fn the_cache_spends_at_most_the_budget_and_not_twice_a_day() {
        let mut c = InspectionCache::default();
        c.roll("2026-10-21", 1_000_000.0);
        let urls: Vec<String> = (0..5).map(|i| format!("https://example.com/{i}")).collect();
        assert_eq!(c.due(&urls, 1_000_000.0).len(), 5);
        let ok = parse_inspection(INSPECTED);
        c.record(&urls[0], 1_000_000.0, INSPECTED.into(), ok.clone());
        c.record(&urls[1], 1_000_000.0, String::new(), Err("HTTP 400 · outside the property".into()));
        assert_eq!(c.used, 2);
        // Within a day, neither is due again - an error included, since
        // asking again would only spend another inspection on the same 400.
        assert_eq!(c.due(&urls, 1_000_000.0 + 3600.0), urls[2..].to_vec());
        // A day on, both are.
        assert_eq!(c.due(&urls, 1_000_000.0 + REINSPECT_AFTER).len(), 5);
        // The budget caps what is due.
        c.used = DAILY_INSPECTIONS - 1;
        assert_eq!(c.due(&urls, 1_000_000.0).len(), 1);
        c.used = DAILY_INSPECTIONS;
        assert!(c.due(&urls, 1_000_000.0).is_empty());
        assert_eq!(c.left(), 0);
        // Google's day turns over and the count with it; old entries go.
        c.roll("2026-10-22", 1_000_000.0 + 15.0 * 86400.0);
        assert_eq!(c.used, 0);
        assert!(c.pages.is_empty());
    }

    #[test]
    fn a_reservation_holds_the_last_inspection_and_the_page() {
        let mut c = InspectionCache::default();
        c.roll("2026-10-21", 10.0);
        c.used = DAILY_INSPECTIONS - 1;
        assert_eq!(c.reserve("https://example.com/a"), Ok(()));
        // Two presses, or a press and the poller: the second is refused.
        assert!(c.reserve("https://example.com/b").unwrap_err().contains("budget spent"));
        assert!(c.due(&["https://example.com/a".to_string()], 10.0).is_empty());
        // The answer is counted once.
        c.record("https://example.com/a", 11.0, INSPECTED.into(), parse_inspection(INSPECTED));
        assert_eq!(c.used, DAILY_INSPECTIONS);
        assert!(c.inflight.is_empty());
        // A request never sent gives its reservation back.
        c.used = 0;
        c.reserve("https://example.com/c").unwrap();
        assert_eq!(c.reserve("https://example.com/c"), Err("already being inspected".to_string()));
        c.release("https://example.com/c");
        assert_eq!(c.used, 0);
    }

    #[test]
    fn the_cache_round_trips_through_its_file() {
        let mut c = InspectionCache::default();
        c.roll("2026-10-21", 10.0);
        c.record("https://example.com/a", 10.0, INSPECTED.into(), parse_inspection(INSPECTED));
        c.record("https://example.com/b", 11.0, "{}".into(), Err("HTTP 400 · outside".into()));
        let back = parse_inspection_cache(&c.to_json());
        assert_eq!(back, c);
        assert_eq!(parse_inspection_cache("not json"), InspectionCache::default());
    }

    #[test]
    fn the_inspection_set_is_top_pages_then_dropped_ones() {
        let cur = vec![row("/a", 1.0, 50.0, 1.0), row("/b", 1.0, 500.0, 1.0), row("/c", 1.0, 5.0, 1.0)];
        let prior = vec![row("/x", 1.0, 10.0, 1.0), row("/y", 1.0, 99.0, 1.0)];
        let m = join_movers(&cur, &prior);
        let (set, tops, dropped) = inspection_set(&m, 2);
        assert_eq!(set, vec!["/b", "/a", "/y", "/x"]);
        assert_eq!((tops, dropped), (2, 2));
        assert_eq!(inspection_set(&m, 0), (Vec::new(), 0, 0));
    }

    #[test]
    fn site_forms_are_named_when_wrong() {
        assert_eq!(site_form_problem("sc-domain:example.com"), None);
        assert_eq!(site_form_problem("https://example.com/"), None);
        assert!(site_form_problem("https://example.com").unwrap().contains("trailing slash"));
        assert!(site_form_problem("example.com").unwrap().contains("sc-domain:example.com"));
        assert!(site_form_problem("sc-domain:https://example.com/").is_some());
        assert_ne!(cache_name("sc-domain:example.com"), cache_name("https://example.com/"));
        assert_ne!(cache_name("https://example.com/a/b/"), cache_name("https://example.com/a_b/"));
        assert!(cache_name("sc-domain:example.com").starts_with("sc-domain_example.com-"));
        assert_eq!(site_host("https://www.example.com/blog/"), "example.com");
        assert_eq!(site_host("sc-domain:Example.com"), "example.com");
    }

    #[test]
    fn pages_are_shortened_only_on_the_property_host() {
        assert_eq!(short_page("sc-domain:example.com", "https://www.example.com/docs"), "/docs");
        assert_eq!(short_page("sc-domain:example.com", "https://example.com"), "/");
        assert_eq!(short_page("sc-domain:example.com", "https://blog.example.com/x"), "blog.example.com/x");
        assert_eq!(short_page("https://example.com/", "https://example.com/pricing"), "/pricing");
        assert_eq!(short_page("sc-domain:example.com", "https://example.com.invalid/"), "example.com.invalid/");
    }
}
