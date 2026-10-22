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

//! The requests this widget makes, and the one place they are sent from.
//!
//! Every Data API request asks for `returnPropertyQuota`, because reading
//! the quota is what lets the widget slow down before Google stops it.
//! The bodies are built by plain functions so their shape is tested
//! without a network.
#![allow(dead_code)]

use opscope_core as tc;
use opscope_core::google::{parse_refusal, Credential, Refusal, SCOPE_ANALYTICS_READONLY};
use serde_json::{json, Value};

const DATA: &str = "https://analyticsdata.googleapis.com/v1beta";
const ADMIN: &str = "https://analyticsadmin.googleapis.com/v1beta";

/// Why a request came back with nothing to draw.
#[derive(Clone, Debug, PartialEq)]
pub struct Fail {
    /// One line, in the wording every Google widget uses.
    pub text: String,
    /// A 429: the last data stays on screen behind a banner.
    pub quota: bool,
}

impl Fail {
    fn plain(text: String) -> Fail {
        Fail { text, quota: false }
    }
}

/// The remedy for a 403 on a property, naming the account to add.
pub fn grant(email: Option<&str>) -> String {
    match email {
        Some(e) => format!("add {} as Viewer under Property access management", e),
        None => "give the token's account Viewer under Property access management".into(),
    }
}

fn bearer(cred: &Credential) -> Result<String, Fail> {
    cred.bearer(&[SCOPE_ANALYTICS_READONLY]).map(|t| t.value).map_err(Fail::plain)
}

/// A refusal in the shared wording, with the pasted-token reminder on a
/// 401 so an expired token is not read as a property gone quiet.
fn refusal(cred: &Credential, status: u16, body: &str, grant: &str) -> Fail {
    let r = parse_refusal(status, body);
    let mut text = r.wording(grant);
    if matches!(r, Refusal::Unauthenticated { .. }) {
        if let Some(note) = cred.warning().filter(|_| matches!(cred, Credential::Token { .. })) {
            text = format!("{} · {}", text, note);
        }
    }
    Fail { text, quota: matches!(r, Refusal::Quota { .. }) }
}

fn post(cred: &Credential, url: &str, body: &Value, grant: &str) -> Result<String, Fail> {
    let token = bearer(cred)?;
    let auth = format!("Bearer {}", token);
    let (status, text, _) = tc::post_answer(
        url,
        &[("Authorization", auth.as_str()), ("Content-Type", "application/json")],
        &body.to_string(),
        30,
    )
    .map_err(|e| Fail::plain(format!("could not reach GA: {}", e)))?;
    if (200..300).contains(&status) {
        Ok(text)
    } else {
        Err(refusal(cred, status, &text, grant))
    }
}

fn get(cred: &Credential, url: &str, grant: &str) -> Result<String, Fail> {
    let token = bearer(cred)?;
    let auth = format!("Bearer {}", token);
    let (status, text, _) = tc::get_answer(url, &[("Authorization", auth.as_str())], 30)
        .map_err(|e| Fail::plain(format!("could not reach GA: {}", e)))?;
    if (200..300).contains(&status) {
        Ok(text)
    } else {
        Err(refusal(cred, status, &text, grant))
    }
}

pub fn realtime(cred: &Credential, id: &str, body: &Value, grant: &str) -> Result<String, Fail> {
    post(cred, &format!("{}/properties/{}:runRealtimeReport", DATA, id), body, grant)
}

pub fn batch(cred: &Credential, id: &str, body: &Value, grant: &str) -> Result<String, Fail> {
    post(cred, &format!("{}/properties/{}:batchRunReports", DATA, id), body, grant)
}

pub fn report(cred: &Credential, id: &str, body: &Value, grant: &str) -> Result<String, Fail> {
    post(cred, &format!("{}/properties/{}:runReport", DATA, id), body, grant)
}

pub fn account_summaries(cred: &Credential, page: Option<&str>, grant: &str) -> Result<String, Fail> {
    let mut url = format!("{}/accountSummaries?pageSize=200", ADMIN);
    if let Some(p) = page {
        url.push_str(&format!("&pageToken={}", p));
    }
    get(cred, &url, grant)
}

pub fn property(cred: &Credential, id: &str, grant: &str) -> Result<String, Fail> {
    get(cred, &format!("{}/properties/{}", ADMIN, id), grant)
}

pub fn key_event_list(cred: &Credential, id: &str, page: Option<&str>, grant: &str) -> Result<String, Fail> {
    let mut url = format!("{}/properties/{}/keyEvents?pageSize=200", ADMIN, id);
    if let Some(p) = page {
        url.push_str(&format!("&pageToken={}", p));
    }
    get(cred, &url, grant)
}

fn dims(names: &[&str]) -> Value {
    Value::Array(names.iter().map(|n| json!({ "name": n })).collect())
}

fn mets(names: &[&str]) -> Value {
    dims(names)
}

/// Totals over the realtime window: one row, no dimensions.
pub fn realtime_totals_body() -> Value {
    json!({
        "metrics": mets(&["activeUsers", "screenPageViews", "keyEvents"]),
        "returnPropertyQuota": true,
    })
}

/// The per-minute series.
pub fn realtime_minutes_body() -> Value {
    json!({
        "dimensions": dims(&["minutesAgo"]),
        "metrics": mets(&["activeUsers", "screenPageViews"]),
        "limit": 30,
        "returnPropertyQuota": true,
    })
}

/// One realtime breakdown: pages by title, devices, countries or events.
pub fn realtime_breakdown_body(dim: &str, metrics: &[&str], limit: u32) -> Value {
    json!({
        "dimensions": dims(&[dim]),
        "metrics": mets(metrics),
        "orderBys": [{ "metric": { "metricName": metrics[0] }, "desc": true }],
        "limit": limit,
        "returnPropertyQuota": true,
    })
}

/// The realtime event breakdown, asked only for the events on screen -
/// the configured list, or those GA flags - so a low-volume key event is
/// never cut off below the limit by busier ordinary events.
pub fn realtime_events_body(configured: &[String]) -> Value {
    let mut body = realtime_breakdown_body("eventName", &["eventCount", "keyEvents"], 250);
    let (key, filter) = key_event_filter(configured);
    body[key] = filter;
    body
}

fn range(start: &str, end: &str, name: &str) -> Value {
    json!({ "startDate": start, "endDate": end, "name": name })
}

/// The current window (`days` complete days to yesterday) and the equal
/// window before it.
pub fn windows(days: usize) -> Value {
    json!([
        range(&format!("{}daysAgo", days), "yesterday", "cur"),
        range(&format!("{}daysAgo", days * 2), &format!("{}daysAgo", days + 1), "prev"),
    ])
}

/// Today, and the same weekday one, two and three weeks back - four
/// ranges, which is the most one request takes.
pub fn same_weekdays() -> Value {
    json!([
        range("today", "today", "today"),
        range("7daysAgo", "7daysAgo", "w1"),
        range("14daysAgo", "14daysAgo", "w2"),
        range("21daysAgo", "21daysAgo", "w3"),
    ])
}

/// The key events asked for: the list named in config by `eventCount`,
/// or every event GA flags, by `keyEvents`.
pub fn key_event_filter(names: &[String]) -> (&'static str, Value) {
    if names.is_empty() {
        (
            "metricFilter",
            json!({ "filter": { "fieldName": "keyEvents",
                "numericFilter": { "operation": "GREATER_THAN", "value": { "int64Value": "0" } } } }),
        )
    } else {
        (
            "dimensionFilter",
            json!({ "filter": { "fieldName": "eventName", "inListFilter": { "values": names } } }),
        )
    }
}

/// The five processed reports one `batchRunReports` carries, in order:
/// hourly, sources today, key events, pages by day (the charts), and
/// source/medium over each whole window.
///
/// Users are a distinct count, so a figure for a window has to be asked
/// for over the window: summing daily users counts a visitor once for
/// every day they came. Pages by day are for drawing days; the window
/// totals for pages and channels come from `window_body`.
pub fn processed_body(window: usize, configured: &[String]) -> Value {
    let (filter_key, filter) = key_event_filter(configured);
    let mut key_events = json!({
        "dateRanges": [range("7daysAgo", "today", "week")],
        "dimensions": dims(&["date", "hour", "eventName"]),
        "metrics": mets(&["keyEvents", "eventCount"]),
        "limit": 10000,
        "returnPropertyQuota": true,
    });
    key_events[filter_key] = filter;
    json!({ "requests": [
        {
            "dateRanges": same_weekdays(),
            "dimensions": dims(&["hour"]),
            "metrics": mets(&["sessions", "totalUsers"]),
            "limit": 200,
            "returnPropertyQuota": true,
        },
        {
            "dateRanges": [range("today", "today", "today"), range("7daysAgo", "7daysAgo", "w1")],
            "dimensions": dims(&["sessionDefaultChannelGroup", "sessionSourceMedium", "hour"]),
            "metrics": mets(&["sessions"]),
            "limit": 10000,
            "returnPropertyQuota": true,
        },
        key_events,
        {
            "dateRanges": windows(window),
            "dimensions": dims(&["pagePath", "date"]),
            "metrics": mets(&["screenPageViews", "totalUsers", "sessions"]),
            "orderBys": [{ "metric": { "metricName": "screenPageViews" }, "desc": true }],
            "limit": 10000,
            "returnPropertyQuota": true,
        },
        {
            "dateRanges": windows(window),
            "dimensions": dims(&["sessionSourceMedium"]),
            "metrics": mets(&["screenPageViews", "totalUsers", "sessions"]),
            "orderBys": [{ "metric": { "metricName": "screenPageViews" }, "desc": true }],
            "limit": 10000,
            "returnPropertyQuota": true,
        },
    ]})
}

/// The second batch, in order: pages and channels over each whole window,
/// and organic landing pages.
pub fn window_body(window: usize) -> Value {
    json!({ "requests": [
        {
            "dateRanges": windows(window),
            "dimensions": dims(&["pagePath"]),
            "metrics": mets(&["screenPageViews", "totalUsers", "sessions"]),
            "orderBys": [{ "metric": { "metricName": "screenPageViews" }, "desc": true }],
            "limit": 10000,
            "returnPropertyQuota": true,
        },
        {
            "dateRanges": windows(window),
            "dimensions": dims(&["sessionDefaultChannelGroup"]),
            "metrics": mets(&["screenPageViews", "totalUsers", "sessions"]),
            "limit": 1000,
            "returnPropertyQuota": true,
        },
        organic_body(window),
    ]})
}

/// Landing pages for Google Search visitors, this window and the last.
pub fn organic_body(window: usize) -> Value {
    json!({
        "dateRanges": windows(window),
        "dimensions": dims(&["landingPage"]),
        "metrics": mets(&["sessions", "keyEvents"]),
        "dimensionFilter": { "filter": { "fieldName": "sessionDefaultChannelGroup",
            "stringFilter": { "matchType": "EXACT", "value": "Organic Search" } } },
        "orderBys": [{ "metric": { "metricName": "sessions" }, "desc": true }],
        "limit": 10000,
        "returnPropertyQuota": true,
    })
}

fn exact(field: &str, value: &str) -> Value {
    json!({ "filter": { "fieldName": field, "stringFilter": { "matchType": "EXACT", "value": value } } })
}

/// A page's detail: 28 days by day against the 28 before, then its
/// sources, countries, devices and key events over the last 28 days.
pub fn page_detail_body(path: &str) -> Value {
    let filter = exact("pagePath", path);
    let last28 = json!([range("28daysAgo", "yesterday", "cur")]);
    let one = |dim: &str, metric: &str| {
        json!({
            "dateRanges": last28.clone(),
            "dimensions": dims(&[dim]),
            "metrics": mets(&[metric]),
            "dimensionFilter": filter.clone(),
            "orderBys": [{ "metric": { "metricName": metric }, "desc": true }],
            "limit": 10,
            "returnPropertyQuota": true,
        })
    };
    json!({ "requests": [
        {
            "dateRanges": [range("28daysAgo", "yesterday", "cur"), range("56daysAgo", "29daysAgo", "prev")],
            "dimensions": dims(&["date"]),
            "metrics": mets(&["screenPageViews", "totalUsers"]),
            "dimensionFilter": filter.clone(),
            "limit": 200,
            "returnPropertyQuota": true,
        },
        one("sessionSourceMedium", "screenPageViews"),
        one("country", "screenPageViews"),
        one("deviceCategory", "screenPageViews"),
        one("eventName", "keyEvents"),
    ]})
}

/// A page's 28-day views and users against the 28 before, asked for over
/// each whole window: users are distinct, so the by-day rows cannot be
/// summed into them. A batch of its own, because the page detail's first
/// batch already holds the five a request may carry.
pub fn page_totals_body(path: &str) -> Value {
    json!({ "requests": [{
        "dateRanges": [range("28daysAgo", "yesterday", "cur"), range("56daysAgo", "29daysAgo", "prev")],
        "metrics": mets(&["screenPageViews", "totalUsers"]),
        "dimensionFilter": exact("pagePath", path),
        "returnPropertyQuota": true,
    }]})
}

/// Everything a page's detail asks for, as the batches it is sent in.
pub fn page_detail_bodies(path: &str) -> Vec<Value> {
    vec![page_detail_body(path), page_totals_body(path)]
}

/// A key event's detail: by day over 28 days, by source today, and by
/// page over 28 days.
pub fn event_detail_body(name: &str, metric: &str) -> Value {
    let filter = exact("eventName", name);
    json!({ "requests": [
        {
            "dateRanges": [range("27daysAgo", "today", "cur")],
            "dimensions": dims(&["date"]),
            "metrics": mets(&[metric]),
            "dimensionFilter": filter.clone(),
            "limit": 100,
            "returnPropertyQuota": true,
        },
        {
            "dateRanges": [range("today", "today", "today")],
            "dimensions": dims(&["sessionSourceMedium"]),
            "metrics": mets(&[metric]),
            "dimensionFilter": filter.clone(),
            "orderBys": [{ "metric": { "metricName": metric }, "desc": true }],
            "limit": 10,
            "returnPropertyQuota": true,
        },
        {
            "dateRanges": [range("27daysAgo", "today", "cur")],
            "dimensions": dims(&["pagePath"]),
            "metrics": mets(&[metric]),
            "dimensionFilter": filter,
            "orderBys": [{ "metric": { "metricName": metric }, "desc": true }],
            "limit": 10,
            "returnPropertyQuota": true,
        },
    ]})
}

/// The property's realtime page in GA's own UI, for the copy key.
pub fn ga_url(id: &str) -> String {
    format!("https://analytics.google.com/analytics/web/#/p{}/realtime/overview", id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn every_request(v: &Value) -> Vec<Value> {
        match v.get("requests").and_then(|r| r.as_array()) {
            Some(r) => r.clone(),
            None => vec![v.clone()],
        }
    }

    #[test]
    fn every_request_asks_for_the_quota() {
        let bodies = [
            realtime_totals_body(),
            realtime_minutes_body(),
            realtime_breakdown_body("unifiedScreenName", &["activeUsers"], 5),
            realtime_events_body(&[]),
            processed_body(7, &[]),
            window_body(7),
            organic_body(14),
            page_detail_body("/pricing"),
            page_totals_body("/pricing"),
            event_detail_body("purchase", "keyEvents"),
        ];
        for body in bodies {
            for r in every_request(&body) {
                assert_eq!(r["returnPropertyQuota"], true, "{}", r);
            }
        }
    }

    #[test]
    fn a_batch_holds_the_five_reports_googles_limit_allows() {
        let b = processed_body(7, &[]);
        let r = b["requests"].as_array().unwrap();
        assert_eq!(r.len(), 5);
        assert_eq!(r[0]["dateRanges"].as_array().unwrap().len(), 4, "four ranges is the API's maximum");
        assert_eq!(r[0]["dateRanges"][3]["startDate"], "21daysAgo");
        assert!(r[2].get("metricFilter").is_some(), "flagged key events by default");
        let named = processed_body(7, &["purchase".into()]);
        assert_eq!(named["requests"][2]["dimensionFilter"]["filter"]["inListFilter"]["values"][0], "purchase");
        assert!(named["requests"][2].get("metricFilter").is_none());
        let p = page_detail_body("/x");
        assert_eq!(p["requests"].as_array().unwrap().len(), 5);
    }

    #[test]
    fn window_figures_are_asked_for_without_a_date() {
        // A distinct count summed over days counts a returning visitor
        // once a day, so no window figure may be built from `date` rows.
        let b = window_body(7);
        let r = b["requests"].as_array().unwrap();
        assert_eq!(r.len(), 3);
        for req in r.iter().chain(processed_body(7, &[])["requests"].as_array().unwrap()[4..].iter()) {
            let names: Vec<&str> = req["dimensions"].as_array().unwrap().iter().map(|d| d["name"].as_str().unwrap()).collect();
            assert!(!names.contains(&"date"), "{:?}", names);
        }
        assert_eq!(r[0]["dimensions"][0]["name"], "pagePath");
        assert_eq!(r[1]["dimensions"][0]["name"], "sessionDefaultChannelGroup");
        assert_eq!(r[2]["dimensions"][0]["name"], "landingPage");
    }

    #[test]
    fn realtime_events_ask_only_for_the_events_shown() {
        let flagged = realtime_events_body(&[]);
        assert_eq!(flagged["metricFilter"]["filter"]["fieldName"], "keyEvents");
        let named = realtime_events_body(&["purchase".into()]);
        assert_eq!(named["dimensionFilter"]["filter"]["inListFilter"]["values"][0], "purchase");
        assert!(named.get("metricFilter").is_none());
    }

    #[test]
    fn the_windows_are_equal_and_end_yesterday() {
        let w = windows(7);
        assert_eq!(w[0], json!({"startDate":"7daysAgo","endDate":"yesterday","name":"cur"}));
        assert_eq!(w[1], json!({"startDate":"14daysAgo","endDate":"8daysAgo","name":"prev"}));
        let w = windows(28);
        assert_eq!(w[1]["startDate"], "56daysAgo");
        assert_eq!(w[1]["endDate"], "29daysAgo");
    }

    #[test]
    fn organic_is_filtered_to_google_search() {
        let o = organic_body(7);
        assert_eq!(o["dimensions"][0]["name"], "landingPage");
        assert_eq!(o["dimensionFilter"]["filter"]["fieldName"], "sessionDefaultChannelGroup");
        assert_eq!(o["dimensionFilter"]["filter"]["stringFilter"]["value"], "Organic Search");
    }

    #[test]
    fn the_403_remedy_names_the_account() {
        assert_eq!(
            grant(Some("reader@example-project.iam.gserviceaccount.com")),
            "add reader@example-project.iam.gserviceaccount.com as Viewer under Property access management"
        );
        assert!(grant(None).contains("Viewer"));
        assert_eq!(ga_url("123"), "https://analytics.google.com/analytics/web/#/p123/realtime/overview");
    }
}
