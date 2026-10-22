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

//! Search Console over HTTPS: the requests, and one site's whole reading.
//!
//! Every request goes through core's curl helpers with the bearer token on
//! stdin, and every refusal is worded by `opscope_core::google`, so this
//! widget says "API not enabled" and "429 quota" in the same words as
//! `google-analytics` does.

use chrono::{Duration, NaiveDate};
use opscope_core as tc;
use opscope_core::google;

use crate::parse::*;

const API: &str = "https://www.googleapis.com/webmasters/v3";
const INSPECT: &str = "https://searchconsole.googleapis.com/v1/urlInspection/index:inspect";
/// Hours of hourly data asked for. Google keeps about ten days of it, and
/// the comparison needs seven days plus the last twenty-four hours.
pub const HOURLY_DAYS: i64 = 9;

/// The status a 2xx answer that could not be read is given: Google was
/// reached, and counted the request, so it is not 0.
pub const UNREADABLE: u16 = 200;

/// A refused or failed request, worded, with its status kept so the caller
/// can tell a 403 that ends the site's round from a 400 on one row.
#[derive(Clone, Debug, PartialEq)]
pub struct Fail {
    /// 0 when Google was never reached; `UNREADABLE` when it answered
    /// 2xx with something that could not be read.
    pub status: u16,
    pub said: String,
}

/// A signed-in caller: the bearer token, and the sentence that tells
/// somebody how to grant the account access - which only this widget
/// knows, since it names Search Console's own screen.
pub struct Client {
    pub bearer: String,
    pub email: Option<String>,
}

impl Client {
    /// What to do about a 403 on a property.
    pub fn grant(&self, site: &str) -> String {
        grant_for(self.email.as_deref(), site)
    }
}

/// "add <email> as a Restricted user…", or the same without an address
/// when the credential is a pasted token and nobody knows whose it is.
pub fn grant_for(email: Option<&str>, site: &str) -> String {
    let who = email.unwrap_or("the account the token belongs to");
    format!(
        "add {} as a Restricted user on {} in Search Console (Settings, Users and permissions)",
        who, site
    )
}

/// Percent-encode a path segment or query value. Everything but the
/// unreserved set is encoded, so `sc-domain:` and `https://…/` survive.
pub fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

fn answered(status: u16, body: String, grant: &str) -> Result<String, Fail> {
    if (200..300).contains(&status) {
        return Ok(body);
    }
    Err(Fail { status, said: google::parse_refusal(status, &body).wording(grant) })
}

fn get(c: &Client, url: &str, site: &str) -> Result<String, Fail> {
    let auth = format!("Bearer {}", c.bearer);
    let (status, body, _) = tc::get_answer(url, &[("Authorization", &auth)], 30)
        .map_err(|e| Fail { status: 0, said: format!("could not reach Google: {}", e) })?;
    answered(status, body, &c.grant(site))
}

fn post(c: &Client, url: &str, body: &str, site: &str) -> Result<String, Fail> {
    let auth = format!("Bearer {}", c.bearer);
    let (status, body, _) = tc::post_answer(
        url,
        &[("Authorization", &auth), ("Content-Type", "application/json")],
        body,
        45,
    )
    .map_err(|e| Fail { status: 0, said: format!("could not reach Google: {}", e) })?;
    answered(status, body, &c.grant(site))
}

/// `sites.list`.
pub fn sites(c: &Client) -> Result<Vec<Site>, Fail> {
    let body = get(c, &format!("{}/sites", API), "the property")?;
    parse_sites(&body).map_err(|said| Fail { status: 0, said })
}

/// `sitemaps.list`, for a site or for the children of one sitemap index.
pub fn sitemaps(c: &Client, site: &str, index: Option<&str>) -> Result<Vec<Sitemap>, Fail> {
    let mut url = format!("{}/sites/{}/sitemaps", API, encode(site));
    if let Some(parent) = index {
        url.push_str(&format!("?sitemapIndex={}", encode(parent)));
    }
    let body = get(c, &url, site)?;
    parse_sitemaps(&body).map_err(|said| Fail { status: 0, said })
}

/// One `searchAnalytics/query` request.
#[derive(Clone, Debug)]
pub struct Query<'a> {
    pub from: NaiveDate,
    pub to: NaiveDate,
    pub dimensions: &'a [&'a str],
    pub kind: &'a str,
    pub state: &'a str,
    pub rows: usize,
    /// One `equals` filter, as (dimension, expression).
    pub filter: Option<(&'a str, &'a str)>,
}

/// The JSON body for a query, apart from the network so it can be tested.
pub fn query_body(q: &Query) -> String {
    let mut body = serde_json::json!({
        "startDate": q.from.format("%Y-%m-%d").to_string(),
        "endDate": q.to.format("%Y-%m-%d").to_string(),
        "dimensions": q.dimensions,
        "type": q.kind,
        "dataState": q.state,
        "rowLimit": q.rows,
    });
    if let Some((dimension, expression)) = q.filter {
        body["dimensionFilterGroups"] = serde_json::json!([{
            "filters": [{ "dimension": dimension, "operator": "equals", "expression": expression }]
        }]);
    }
    body.to_string()
}

pub fn query(c: &Client, site: &str, q: &Query) -> Result<Analytics, Fail> {
    let url = format!("{}/sites/{}/searchAnalytics/query", API, encode(site));
    let body = post(c, &url, &query_body(q), site)?;
    parse_search_analytics(&body).map_err(|said| Fail { status: 0, said })
}

/// `urlInspection/index:inspect` for one page. The raw answer comes back
/// with the parsed one so the cache keeps what Google said.
pub fn inspect(c: &Client, site: &str, page: &str) -> Result<(String, Inspection), Fail> {
    let body = serde_json::json!({ "inspectionUrl": page, "siteUrl": site }).to_string();
    let raw = post(c, INSPECT, &body, site)?;
    // Google answered 2xx and counted the request; only the reading
    // failed. Status 0 means never reached and gives the reservation back,
    // so this must not be 0.
    let parsed = parse_inspection(&raw).map_err(|said| Fail { status: UNREADABLE, said })?;
    Ok((raw, parsed))
}

// --------------------------------------------------------------- a site --

/// What a section of the pane holds: a value, the reason there is none,
/// or both - a value kept from an earlier round with the error that kept
/// the newer one from arriving, which is drawn as a stale banner.
#[derive(Clone, Debug, PartialEq)]
pub struct Part<T> {
    pub value: Option<T>,
    /// Why there is no value, when that is not an error: "no final days
    /// yet", "Google has no queries for Discover".
    pub note: Option<String>,
    pub error: Option<String>,
    /// When the value was fetched, Unix seconds.
    pub at: f64,
}

impl<T> Default for Part<T> {
    fn default() -> Self {
        Part { value: None, note: None, error: None, at: 0.0 }
    }
}

impl<T> Part<T> {
    pub fn ok(value: T, at: f64) -> Self {
        Part { value: Some(value), note: None, error: None, at }
    }
    pub fn failed(said: impl Into<String>) -> Self {
        Part { value: None, note: None, error: Some(said.into()), at: 0.0 }
    }
    pub fn noted(said: impl Into<String>) -> Self {
        Part { value: None, note: Some(said.into()), error: None, at: 0.0 }
    }
    /// A round that failed keeps the last value it replaced, so the pane
    /// shows that reading under a banner naming its age and the error,
    /// rather than blanking or passing it off as current.
    pub fn after(self, old: Part<T>) -> Part<T> {
        if self.value.is_none() && self.error.is_some() && old.value.is_some() {
            return Part { value: old.value, note: None, error: self.error, at: old.at };
        }
        self
    }
}

/// The movers for one dimension, and whether Google's row limit cut them.
#[derive(Clone, Debug, PartialEq)]
pub struct Movers {
    pub rows: Vec<Mover>,
    /// Either window came back with exactly `MOVER_ROWS` rows, so there
    /// are more Google did not send: "top 1,000 of more".
    pub capped: bool,
}

/// What a refresh is asked for. `m` only re-sorts and is not here; `w`,
/// `d` and `t` change what Google is asked, so they are.
#[derive(Clone, Debug, PartialEq)]
pub struct Params {
    pub fresh: bool,
    pub days: i64,
    pub kind: String,
}

impl Params {
    pub fn state(&self) -> &'static str {
        if self.fresh { "all" } else { "final" }
    }
}

/// Where final and fresh data end, and the property's daily totals.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Days {
    pub final_end: Option<NaiveDate>,
    pub fresh_end: Option<NaiveDate>,
    pub daily: Vec<(NaiveDate, Metrics)>,
}

/// One property's whole reading.
#[derive(Clone, Debug, PartialEq)]
pub struct SiteData {
    pub site: String,
    /// From `sites.list`; empty when the list did not name this site.
    pub permission: String,
    /// A reason nothing at all was asked for this site: not listed, an
    /// unverified user, a 403 on the property.
    pub problem: Option<String>,
    pub params: Params,
    pub hours: Part<HourView>,
    /// The last final day, the last day with any data, and the daily
    /// totals they were read from.
    pub days: Part<Days>,
    /// The two windows the movers and terms compare, when there is a day
    /// to end them on.
    pub windows: Option<((NaiveDate, NaiveDate), (NaiveDate, NaiveDate))>,
    pub pages: Part<Movers>,
    pub queries: Part<Movers>,
    pub terms: Part<Vec<Term>>,
    pub sitemaps: Part<Vec<Sitemap>>,
    /// Why this round inspected less than it meant to: a 429, a budget
    /// spent. Absent when it inspected everything due.
    pub inspect_error: Option<String>,
    pub fetched: f64,
}

impl SiteData {
    pub fn new(site: &str, params: &Params) -> SiteData {
        SiteData {
            site: site.to_string(),
            permission: String::new(),
            problem: None,
            params: params.clone(),
            hours: Part::default(),
            days: Part::default(),
            windows: None,
            pages: Part::default(),
            queries: Part::default(),
            terms: Part::default(),
            sitemaps: Part::default(),
            inspect_error: None,
            fetched: 0.0,
        }
    }

    /// Fold a new round over the last one. Values for different
    /// parameters are never kept: a 28-day reading is not a stale 7-day
    /// one. Nor are movers and terms kept across a window that moved on:
    /// they would be drawn under the new window's dates, which they never
    /// covered, so a failed section after the day turns over shows its
    /// error alone.
    pub fn after(self, old: SiteData) -> SiteData {
        if old.params != self.params || old.site != self.site {
            return self;
        }
        let same_window = self.windows.is_none() || self.windows == old.windows;
        let keep = |new: Part<_>, old: Part<_>| if same_window { new.after(old) } else { new };
        SiteData {
            hours: self.hours.after(old.hours),
            days: self.days.after(old.days),
            windows: self.windows.or(old.windows),
            pages: keep(self.pages, old.pages),
            queries: keep(self.queries, old.queries),
            terms: if same_window { self.terms.after(old.terms) } else { self.terms },
            sitemaps: self.sitemaps.after(old.sitemaps),
            ..self
        }
    }
}

/// Whether a refusal is about the whole property, so asking it anything
/// else this round would only be refused the same way.
fn ends_the_round(f: &Fail) -> bool {
    f.status == 401 || f.status == 403
}

fn movers(c: &Client, site: &str, dim: &str, p: &Params, w: ((NaiveDate, NaiveDate), (NaiveDate, NaiveDate))) -> Result<Movers, Fail> {
    let ask = |(from, to): (NaiveDate, NaiveDate)| {
        query(c, site, &Query {
            from,
            to,
            dimensions: &[dim],
            kind: &p.kind,
            state: p.state(),
            rows: MOVER_ROWS,
            filter: None,
        })
    };
    let cur = ask(w.0)?;
    let prior = ask(w.1)?;
    Ok(Movers {
        capped: cur.rows.len() >= MOVER_ROWS || prior.rows.len() >= MOVER_ROWS,
        rows: join_movers(&cur.rows, &prior.rows),
    })
}

fn terms(c: &Client, site: &str, p: &Params, w: ((NaiveDate, NaiveDate), (NaiveDate, NaiveDate))) -> Result<Vec<Term>, Fail> {
    let ask = |(from, to): (NaiveDate, NaiveDate)| {
        query(c, site, &Query {
            from,
            to,
            dimensions: &["query", "page"],
            kind: &p.kind,
            state: p.state(),
            rows: TERM_ROWS,
            filter: None,
        })
    };
    let cur = ask(w.0)?;
    let prior = ask(w.1)?;
    Ok(group_terms(&cur.rows, &prior.rows))
}

/// Read one property: hourly, the last days, movers, terms and sitemaps.
///
/// `today` is Google's date (Pacific), which is what its date ranges mean.
/// A 401 or 403 ends the round at once and becomes the site's problem.
pub fn fetch_site(c: &Client, site: &str, p: &Params, today: NaiveDate, now: chrono::DateTime<chrono::Utc>) -> SiteData {
    let at = tc::now();
    let mut d = SiteData::new(site, p);
    d.fetched = at;
    macro_rules! stop_on {
        ($fail:expr) => {
            if ends_the_round(&$fail) {
                d.problem = Some($fail.said.clone());
                return d;
            }
        };
    }
    match query(c, site, &Query {
        from: today - Duration::days(HOURLY_DAYS),
        to: today,
        dimensions: &["hour"],
        kind: &p.kind,
        state: "hourly_all",
        rows: MOVER_ROWS,
        filter: None,
    }) {
        Ok(a) => d.hours = Part::ok(hour_view(&a, now), at),
        Err(f) => {
            stop_on!(f);
            d.hours = Part::failed(f.said);
        }
    }
    // The last days: one `date` grouping in `all`, which carries the
    // first incomplete date and so says where final data ends, and where
    // any data ends, in one request.
    let span = 2 * p.days.max(1) + 14;
    let end = match query(c, site, &Query {
        from: today - Duration::days(span),
        to: today,
        dimensions: &["date"],
        kind: &p.kind,
        state: "all",
        rows: MOVER_ROWS,
        filter: None,
    }) {
        Ok(a) => {
            let (final_end, fresh_end) = last_days(&a);
            d.days = Part::ok(Days { final_end, fresh_end, daily: daily(&a) }, at);
            if p.fresh { fresh_end } else { final_end }
        }
        Err(f) => {
            stop_on!(f);
            d.days = Part::failed(f.said.clone());
            d.pages = Part::failed(format!("could not tell which days are final: {}", f.said));
            None
        }
    };
    match end {
        Some(end) => {
            let w = windows(end, p.days);
            d.windows = Some(w);
            match movers(c, site, "page", p, w) {
                Ok(m) => d.pages = Part::ok(m, at),
                Err(f) => {
                    stop_on!(f);
                    d.pages = Part::failed(f.said);
                }
            }
            if p.kind == "discover" {
                let why = "Google reports no search queries for Discover";
                d.queries = Part::noted(why);
                d.terms = Part::noted(why);
            } else {
                match movers(c, site, "query", p, w) {
                    Ok(m) => d.queries = Part::ok(m, at),
                    Err(f) => {
                        stop_on!(f);
                        d.queries = Part::failed(f.said);
                    }
                }
                match terms(c, site, p, w) {
                    Ok(t) => d.terms = Part::ok(t, at),
                    Err(f) => {
                        stop_on!(f);
                        d.terms = Part::failed(f.said);
                    }
                }
            }
        }
        None if d.days.value.is_some() => {
            let why = if p.fresh {
                "no day with data yet for this property"
            } else {
                "no final days yet: Google finalises a day 2-3 days after it ends, and a new property has none"
            };
            d.pages = Part::noted(why);
            d.queries = Part::noted(why);
            d.terms = Part::noted(why);
        }
        None => {
            let said = d.pages.error.clone().unwrap_or_default();
            d.queries = Part::failed(said.clone());
            d.terms = Part::failed(said);
        }
    }
    match sitemaps(c, site, None) {
        Ok(s) => d.sitemaps = Part::ok(s, at),
        Err(f) => {
            stop_on!(f);
            d.sitemaps = Part::failed(f.said);
        }
    }
    d
}

/// What the detail screen fetches for one page, query or sitemap.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Detail {
    /// Daily metrics, oldest first.
    pub series: Vec<(NaiveDate, Metrics)>,
    /// The top queries for a page, or the top pages for a query.
    pub top: Vec<Row>,
    /// A sitemap index's children.
    pub children: Vec<Sitemap>,
    pub children_error: Option<String>,
}

/// Days the detail series covers.
pub const DETAIL_DAYS: i64 = 28;

/// 28 days of one page or query, and what it pairs with: the queries a
/// page ranks for, or the pages ranking for a query.
pub fn fetch_detail(c: &Client, site: &str, p: &Params, dim: &str, key: &str, end: NaiveDate, window: (NaiveDate, NaiveDate)) -> Result<Detail, String> {
    let series = query(c, site, &Query {
        from: end - Duration::days(DETAIL_DAYS - 1),
        to: end,
        dimensions: &["date"],
        kind: &p.kind,
        state: p.state(),
        rows: MOVER_ROWS,
        filter: Some((dim, key)),
    })
    .map_err(|f| f.said)?;
    let other = if dim == "page" { "query" } else { "page" };
    let top = if p.kind == "discover" && other == "query" {
        Vec::new()
    } else {
        query(c, site, &Query {
            from: window.0,
            to: window.1,
            dimensions: &[other],
            kind: &p.kind,
            state: p.state(),
            rows: 10,
            filter: Some((dim, key)),
        })
        .map_err(|f| f.said)?
        .rows
    };
    let mut days: Vec<(NaiveDate, Metrics)> = series
        .rows
        .iter()
        .filter_map(|r| r.keys.first().and_then(|k| parse_date(k)).map(|d| (d, r.m)))
        .collect();
    days.sort_by_key(|(d, _)| *d);
    Ok(Detail { series: days, top, ..Detail::default() })
}

/// A sitemap index's children. A plain sitemap has none to ask for.
pub fn fetch_children(c: &Client, site: &str, path: &str) -> Detail {
    match sitemaps(c, site, Some(path)) {
        Ok(children) => Detail { children, ..Detail::default() },
        Err(f) => Detail { children_error: Some(f.said), ..Detail::default() },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn site_names_are_encoded_whole() {
        assert_eq!(encode("sc-domain:example.com"), "sc-domain%3Aexample.com");
        assert_eq!(encode("https://example.com/"), "https%3A%2F%2Fexample.com%2F");
    }

    #[test]
    fn a_query_body_carries_state_type_and_filter() {
        let q = Query {
            from: parse_date("2026-10-12").unwrap(),
            to: parse_date("2026-10-18").unwrap(),
            dimensions: &["date"],
            kind: "web",
            state: "final",
            rows: 25,
            filter: Some(("page", "https://example.com/a")),
        };
        let v: serde_json::Value = serde_json::from_str(&query_body(&q)).unwrap();
        assert_eq!(v["startDate"], "2026-10-12");
        assert_eq!(v["dataState"], "final");
        assert_eq!(v["rowLimit"], 25);
        assert_eq!(v["dimensionFilterGroups"][0]["filters"][0]["expression"], "https://example.com/a");
    }

    #[test]
    fn a_failed_round_keeps_the_last_reading_behind_its_error() {
        let old: Part<u32> = Part::ok(7, 100.0);
        let kept = Part::<u32>::failed("429 quota").after(old.clone());
        assert_eq!(kept.value, Some(7));
        assert_eq!(kept.at, 100.0);
        assert_eq!(kept.error.as_deref(), Some("429 quota"));
        // A good round replaces it outright.
        assert_eq!(Part::ok(9, 200.0).after(old.clone()).value, Some(9));
        // A note is not an error and keeps nothing.
        assert_eq!(Part::<u32>::noted("no final days").after(old).value, None);
    }

    #[test]
    fn a_reading_for_other_parameters_is_never_kept() {
        let p7 = Params { fresh: false, days: 7, kind: "web".into() };
        let p28 = Params { days: 28, ..p7.clone() };
        let mut old = SiteData::new("sc-domain:example.com", &p7);
        old.sitemaps = Part::ok(Vec::new(), 1.0);
        let mut new = SiteData::new("sc-domain:example.com", &p28);
        new.sitemaps = Part::failed("HTTP 500");
        assert_eq!(new.clone().after(old.clone()).sitemaps.value, None);
        let mut same = SiteData::new("sc-domain:example.com", &p7);
        same.sitemaps = Part::failed("HTTP 500");
        assert_eq!(same.after(old).sitemaps.value, Some(Vec::new()));
    }

    #[test]
    fn movers_are_never_kept_under_a_window_they_did_not_cover() {
        let p = Params { fresh: false, days: 7, kind: "web".into() };
        let movers = Movers { rows: Vec::new(), capped: false };
        let mut old = SiteData::new("sc-domain:example.com", &p);
        old.windows = Some(windows(parse_date("2026-10-17").unwrap(), 7));
        old.pages = Part::ok(movers.clone(), 1.0);
        old.terms = Part::ok(Vec::new(), 1.0);
        // The day moved on and the movers request failed.
        let mut new = SiteData::new("sc-domain:example.com", &p);
        new.windows = Some(windows(parse_date("2026-10-18").unwrap(), 7));
        new.pages = Part::failed("HTTP 500");
        new.terms = Part::failed("HTTP 500");
        let folded = new.clone().after(old.clone());
        assert_eq!(folded.pages.value, None);
        assert_eq!(folded.terms.value, None);
        assert_eq!(folded.pages.error.as_deref(), Some("HTTP 500"));
        // The same window keeps the old reading behind its error.
        new.windows = old.windows;
        assert_eq!(new.after(old).pages.value, Some(movers));
    }

    #[test]
    fn the_grant_names_the_account_or_says_it_cannot() {
        let with = grant_for(Some("reader@example-project.iam.gserviceaccount.com"), "sc-domain:example.com");
        assert!(with.starts_with("add reader@example-project.iam.gserviceaccount.com as a Restricted user"));
        assert!(grant_for(None, "sc-domain:example.com").contains("the account the token belongs to"));
    }
}
