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

//! Parsers for every answer this widget reads, always compiled.
//!
//! GA's Data API answers `runRealtimeReport`, `runReport` and
//! `batchRunReports` in one shape - headers, rows of string values, a
//! metadata block and, when asked, the property's quota - and the Admin
//! API answers the property list, one property, and its key events. The
//! widget's own sample file is the fourth source, and the only one it
//! writes. Every function here takes the text as it arrived.
#![allow(dead_code)]

use serde_json::Value;

/// One row of a report: the dimension values and the metric values, in
/// the order of the report's headers.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Row {
    pub dims: Vec<String>,
    pub mets: Vec<f64>,
}

/// One quota bucket as `propertyQuota` reports it.
///
/// Google defines `consumed` as what *the request that carried it* cost
/// and `remaining` as the bucket's balance after it - not usage so far
/// and not the bucket's size. The size comes from the documented limits,
/// in `documented_limit`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Bucket {
    /// Tokens the one request cost.
    pub consumed: f64,
    /// Tokens left in the bucket after it.
    pub remaining: f64,
}

/// Google's documented size for a token bucket: standard properties, or
/// Analytics 360 at ten times that. Realtime and core reports have the
/// same limits in separate buckets. `None` for a bucket that is not
/// counted in tokens.
pub fn documented_limit(name: &str, premium: bool) -> Option<f64> {
    let standard = match name {
        "tokensPerDay" => 200_000.0,
        "tokensPerHour" => 40_000.0,
        "tokensPerProjectPerHour" => 14_000.0,
        _ => return None,
    };
    Some(if premium { standard * 10.0 } else { standard })
}

/// One token bucket read against its limit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Level {
    /// The bucket's size.
    pub limit: f64,
    /// Left after the last request.
    pub left: f64,
    /// Spent so far in this bucket's period, by every caller of the
    /// property: the limit less what is left.
    pub used: f64,
    /// What the last request alone cost.
    pub last: f64,
}

impl Level {
    pub fn fraction_left(&self) -> f64 {
        if self.limit > 0.0 {
            (self.left / self.limit).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }
}

/// The property's quota, as the response that carried it reported it.
///
/// Realtime and core reports draw on separate quotas, so a widget holds
/// one of these for each.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Quota {
    /// `(name, bucket)` in the order Google sent them.
    pub buckets: Vec<(String, Bucket)>,
}

impl Quota {
    pub fn get(&self, name: &str) -> Option<Bucket> {
        self.buckets.iter().find(|(n, _)| n == name).map(|(_, b)| *b)
    }

    /// Token bucket `name` against its documented limit. A balance above
    /// the standard limit can only be a 360 property, so it is read as
    /// one even before the Admin API has said so.
    pub fn level(&self, name: &str, premium: bool) -> Option<Level> {
        let b = self.get(name)?;
        let mut limit = documented_limit(name, premium)?;
        if b.remaining > limit {
            limit = documented_limit(name, true)?.max(b.remaining);
        }
        Some(Level { limit, left: b.remaining, used: (limit - b.remaining).max(0.0), last: b.consumed })
    }

    /// The token bucket with the least left, as `(name, fraction left)`.
    ///
    /// Only token buckets: `concurrentRequests` is empty between requests
    /// and full during one, and slowing down over it would mean slowing
    /// down for having made a request.
    pub fn tightest(&self, premium: bool) -> Option<(String, f64)> {
        self.buckets
            .iter()
            .filter_map(|(n, _)| self.level(n, premium).map(|l| (n.clone(), l.fraction_left())))
            .min_by(|a, b| a.1.total_cmp(&b.1))
    }
}

/// What GA says about a report beyond its rows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Meta {
    /// Rows were withheld below Google's privacy thresholds.
    pub thresholded: bool,
    /// Rows were folded into `(other)`.
    pub other_row: bool,
    /// The report was computed from a sample.
    pub sampled: bool,
    /// Why the report is empty, when GA says.
    pub empty_reason: String,
    pub time_zone: String,
    pub currency: String,
}

/// One report, realtime or processed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    pub dims: Vec<String>,
    pub mets: Vec<String>,
    pub rows: Vec<Row>,
    /// GA's own count of rows, which may exceed `rows` when a limit cut it.
    pub row_count: usize,
    pub meta: Meta,
    pub quota: Option<Quota>,
}

impl Report {
    pub fn dim(&self, name: &str) -> Option<usize> {
        self.dims.iter().position(|d| d == name)
    }

    pub fn met(&self, name: &str) -> Option<usize> {
        self.mets.iter().position(|m| m == name)
    }

    /// Dimension `name` of `row`, or "" when the report has no such
    /// dimension.
    pub fn d<'a>(&self, row: &'a Row, name: &str) -> &'a str {
        self.dim(name)
            .and_then(|at| row.dims.get(at))
            .map(String::as_str)
            .unwrap_or("")
    }

    /// Metric `name` of `row`, or 0 when the report has no such metric.
    pub fn m(&self, row: &Row, name: &str) -> f64 {
        self.met(name).and_then(|at| row.mets.get(at)).copied().unwrap_or(0.0)
    }
}

fn names(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(|h| h.as_array())
        .map(|h| {
            h.iter()
                .map(|x| x.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn values(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(|h| h.as_array())
        .map(|h| {
            h.iter()
                .map(|x| x.get("value").and_then(|n| n.as_str()).unwrap_or("").to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// A number from a field GA may send as a JSON number or as a string.
fn number(v: Option<&Value>) -> f64 {
    match v {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

fn quota_from(v: &Value) -> Option<Quota> {
    let obj = v.get("propertyQuota")?.as_object()?;
    let mut buckets = Vec::new();
    for (name, b) in obj {
        if !b.is_object() {
            continue;
        }
        buckets.push((
            name.clone(),
            Bucket { consumed: number(b.get("consumed")), remaining: number(b.get("remaining")) },
        ));
    }
    Some(Quota { buckets })
}

fn report_from(v: &Value) -> Report {
    let dims = names(v, "dimensionHeaders");
    let mets = names(v, "metricHeaders");
    let rows: Vec<Row> = v
        .get("rows")
        .and_then(|r| r.as_array())
        .map(|rows| {
            rows.iter()
                .map(|r| Row {
                    dims: values(r, "dimensionValues"),
                    mets: values(r, "metricValues")
                        .iter()
                        .map(|s| s.parse::<f64>().unwrap_or(0.0))
                        .collect(),
                })
                .collect()
        })
        .unwrap_or_default();
    let meta = v.get("metadata").cloned().unwrap_or(Value::Null);
    let flag = |k: &str| meta.get(k).and_then(|b| b.as_bool()).unwrap_or(false);
    let text = |k: &str| meta.get(k).and_then(|b| b.as_str()).unwrap_or("").to_string();
    let sampled = meta
        .get("samplingMetadatas")
        .and_then(|s| s.as_array())
        .is_some_and(|s| !s.is_empty());
    Report {
        row_count: v.get("rowCount").and_then(|c| c.as_u64()).map(|c| c as usize).unwrap_or(rows.len()),
        dims,
        mets,
        rows,
        meta: Meta {
            thresholded: flag("subjectToThresholding"),
            other_row: flag("dataLossFromOtherRow"),
            sampled,
            empty_reason: text("emptyReason"),
            time_zone: text("timeZone"),
            currency: text("currencyCode"),
        },
        quota: quota_from(v),
    }
}

fn json(text: &str, what: &str) -> Result<Value, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("{} is not JSON ({})", what, e))?;
    if !v.is_object() {
        return Err(format!("{} is not a JSON object", what));
    }
    Ok(v)
}

/// One `runReport` or `runRealtimeReport` answer.
pub fn parse_report(text: &str) -> Result<Report, String> {
    Ok(report_from(&json(text, "GA's report")?))
}

/// A `batchRunReports` answer: its reports, in the order they were asked.
pub fn parse_batch(text: &str) -> Result<Vec<Report>, String> {
    let v = json(text, "GA's batch of reports")?;
    Ok(v.get("reports")
        .and_then(|r| r.as_array())
        .map(|r| r.iter().map(report_from).collect())
        .unwrap_or_default())
}

/// A GA4 property the service account can see.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PropertySummary {
    /// The number after `properties/`.
    pub id: String,
    pub name: String,
    pub account: String,
}

/// One page of Admin `accountSummaries`, and the token for the next.
pub fn parse_account_summaries(text: &str) -> Result<(Vec<PropertySummary>, Option<String>), String> {
    let v = json(text, "GA's account list")?;
    let mut out = Vec::new();
    for account in v.get("accountSummaries").and_then(|a| a.as_array()).into_iter().flatten() {
        let account_name = account.get("displayName").and_then(|n| n.as_str()).unwrap_or("");
        for p in account.get("propertySummaries").and_then(|p| p.as_array()).into_iter().flatten() {
            let resource = p.get("property").and_then(|n| n.as_str()).unwrap_or("");
            let id = resource.strip_prefix("properties/").unwrap_or("").to_string();
            if id.is_empty() {
                continue;
            }
            out.push(PropertySummary {
                id,
                name: p.get("displayName").and_then(|n| n.as_str()).unwrap_or("").to_string(),
                account: account_name.to_string(),
            });
        }
    }
    let next = v
        .get("nextPageToken")
        .and_then(|t| t.as_str())
        .filter(|t| !t.is_empty())
        .map(str::to_string);
    Ok((out, next))
}

/// What Admin `properties/{id}` says about one property.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PropertyInfo {
    pub name: String,
    pub time_zone: String,
    pub currency: String,
    /// Analytics 360, whose quota is ten times a standard property's.
    pub premium: bool,
}

pub fn parse_property(text: &str) -> Result<PropertyInfo, String> {
    let v = json(text, "GA's property")?;
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    Ok(PropertyInfo {
        name: s("displayName"),
        time_zone: s("timeZone"),
        currency: s("currencyCode"),
        premium: s("serviceLevel") == "GOOGLE_ANALYTICS_360",
    })
}

/// One page of Admin `properties/{id}/keyEvents`: the event names, and the
/// token for the next page.
pub fn parse_key_event_list(text: &str) -> Result<(Vec<String>, Option<String>), String> {
    let v = json(text, "GA's key event list")?;
    let names = v
        .get("keyEvents")
        .and_then(|k| k.as_array())
        .map(|k| {
            k.iter()
                .filter_map(|e| e.get("eventName").and_then(|n| n.as_str()))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let next = v
        .get("nextPageToken")
        .and_then(|t| t.as_str())
        .filter(|t| !t.is_empty())
        .map(str::to_string);
    Ok((names, next))
}

/// One reading this widget took itself: active users over the last 30
/// minutes, page views over the same window, and each key event's count.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sample {
    /// Unix seconds.
    pub t: f64,
    pub active: f64,
    pub views: f64,
    pub events: Vec<(String, f64)>,
}

/// The sample file, one reading a line: `<unix> <active> <views>` and then
/// `name=count` for each key event. A line that does not read is skipped,
/// so a half-written last line costs one reading rather than the file.
pub fn parse_samples(text: &str) -> Vec<Sample> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let (Some(t), Some(active), Some(views)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let (Ok(t), Ok(active), Ok(views)) = (t.parse::<f64>(), active.parse::<f64>(), views.parse::<f64>())
        else {
            continue;
        };
        if !t.is_finite() || !active.is_finite() || !views.is_finite() {
            continue;
        }
        let events = parts
            .filter_map(|p| p.rsplit_once('='))
            .filter_map(|(n, c)| c.parse::<f64>().ok().map(|c| (n.to_string(), c)))
            .collect();
        out.push(Sample { t, active, views, events });
    }
    out.sort_by(|a, b| a.t.total_cmp(&b.t));
    out
}

/// One line of the sample file. Event names are GA's, which allow only
/// letters, digits and underscores, so nothing here needs escaping; any
/// whitespace or `=` that did arrive is replaced rather than trusted.
pub fn format_sample(s: &Sample) -> String {
    let mut line = format!("{:.0} {} {}", s.t, s.active, s.views);
    for (name, count) in &s.events {
        let clean: String = name
            .chars()
            .map(|c| if c.is_whitespace() || c == '=' { '_' } else { c })
            .collect();
        line.push_str(&format!(" {}={}", clean, count));
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    const REALTIME: &str = r#"{
      "dimensionHeaders": [{"name": "minutesAgo"}],
      "metricHeaders": [{"name": "activeUsers", "type": "TYPE_INTEGER"}, {"name": "screenPageViews", "type": "TYPE_INTEGER"}],
      "rows": [
        {"dimensionValues": [{"value": "00"}], "metricValues": [{"value": "7"}, {"value": "12"}]},
        {"dimensionValues": [{"value": "03"}], "metricValues": [{"value": "4"}, {"value": "5"}]}
      ],
      "rowCount": 2,
      "propertyQuota": {
        "tokensPerDay": {"consumed": 30, "remaining": 199970},
        "tokensPerHour": {"consumed": 30, "remaining": 13970},
        "concurrentRequests": {"remaining": 10},
        "serverErrorsPerProjectPerHour": {"remaining": 10},
        "potentiallyThresholdedRequestsPerHour": {"remaining": 120},
        "tokensPerProjectPerHour": {"consumed": 30, "remaining": 13970}
      },
      "kind": "analyticsData#runRealtimeReport"
    }"#;

    #[test]
    fn a_realtime_report_reads_rows_and_quota() {
        let r = parse_report(REALTIME).unwrap();
        assert_eq!(r.dims, vec!["minutesAgo"]);
        assert_eq!(r.mets, vec!["activeUsers", "screenPageViews"]);
        assert_eq!(r.rows.len(), 2);
        assert_eq!(r.d(&r.rows[1], "minutesAgo"), "03");
        assert_eq!(r.m(&r.rows[0], "screenPageViews"), 12.0);
        assert_eq!(r.m(&r.rows[0], "noSuchMetric"), 0.0);
        let q = r.quota.unwrap();
        assert_eq!(q.get("tokensPerHour"), Some(Bucket { consumed: 30.0, remaining: 13970.0 }));
        assert_eq!(q.get("concurrentRequests"), Some(Bucket { consumed: 0.0, remaining: 10.0 }));
        // 13,970 left of the property's hourly 40,000 is the tightest;
        // the same balance is nearly all of the per-project 14,000.
        assert_eq!(q.tightest(false).map(|t| t.0).as_deref(), Some("tokensPerHour"));
        let l = q.level("tokensPerProjectPerHour", false).unwrap();
        assert_eq!((l.limit, l.left, l.used, l.last), (14_000.0, 13_970.0, 30.0, 30.0));
    }

    #[test]
    fn the_tightest_bucket_ignores_concurrency() {
        let q = Quota {
            buckets: vec![
                ("concurrentRequests".into(), Bucket { consumed: 1.0, remaining: 0.0 }),
                ("tokensPerDay".into(), Bucket { consumed: 10.0, remaining: 10_000.0 }),
                ("tokensPerHour".into(), Bucket { consumed: 10.0, remaining: 39_000.0 }),
            ],
        };
        assert_eq!(q.tightest(false), Some(("tokensPerDay".into(), 0.05)));
        assert_eq!(Quota::default().tightest(false), None);
    }

    #[test]
    fn consumed_is_one_request_not_the_bucket() {
        // A 10-token request with 100 left is a bucket all but empty, not
        // one that is 91% full.
        let q = Quota { buckets: vec![("tokensPerHour".into(), Bucket { consumed: 10.0, remaining: 100.0 })] };
        let l = q.level("tokensPerHour", false).unwrap();
        assert_eq!((l.limit, l.used, l.last), (40_000.0, 39_900.0, 10.0));
        assert!(q.tightest(false).unwrap().1 < 0.01);
        // 360 is ten times the size, said or inferred from the balance.
        assert_eq!(q.level("tokensPerHour", true).unwrap().limit, 400_000.0);
        let big = Quota { buckets: vec![("tokensPerHour".into(), Bucket { consumed: 10.0, remaining: 390_000.0 })] };
        assert_eq!(big.level("tokensPerHour", false).unwrap().limit, 400_000.0);
        assert_eq!(q.level("concurrentRequests", false), None);
    }

    #[test]
    fn a_processed_report_reads_its_metadata() {
        let text = r#"{
          "dimensionHeaders": [{"name": "pagePath"}, {"name": "dateRange"}],
          "metricHeaders": [{"name": "screenPageViews", "type": "TYPE_INTEGER"}],
          "rows": [{"dimensionValues": [{"value": "/pricing"}, {"value": "cur"}], "metricValues": [{"value": "9120"}]}],
          "rowCount": 212,
          "metadata": {"currencyCode": "USD", "timeZone": "America/New_York", "subjectToThresholding": true,
                       "dataLossFromOtherRow": true, "samplingMetadatas": [{"samplesReadCount": "1", "samplingSpaceSize": "2"}]},
          "kind": "analyticsData#runReport"
        }"#;
        let r = parse_report(text).unwrap();
        assert_eq!(r.row_count, 212);
        assert!(r.meta.thresholded && r.meta.other_row && r.meta.sampled);
        assert_eq!(r.meta.time_zone, "America/New_York");
        assert_eq!(r.meta.currency, "USD");
        assert!(r.quota.is_none(), "no quota block is not an empty quota");
    }

    #[test]
    fn an_empty_report_keeps_its_reason() {
        let r = parse_report(r#"{"dimensionHeaders":[{"name":"hour"}],"metricHeaders":[{"name":"sessions"}],"metadata":{"emptyReason":"No data for the requested date ranges."}}"#).unwrap();
        assert!(r.rows.is_empty());
        assert_eq!(r.row_count, 0);
        assert_eq!(r.meta.empty_reason, "No data for the requested date ranges.");
    }

    #[test]
    fn a_batch_keeps_its_order() {
        let text = r#"{"reports":[
          {"dimensionHeaders":[{"name":"hour"}],"metricHeaders":[{"name":"sessions"}],"rows":[{"dimensionValues":[{"value":"01"}],"metricValues":[{"value":"3"}]}]},
          {"dimensionHeaders":[{"name":"eventName"}],"metricHeaders":[{"name":"keyEvents"}],"propertyQuota":{"tokensPerHour":{"consumed":5,"remaining":13995}}}
        ],"kind":"analyticsData#batchRunReports"}"#;
        let b = parse_batch(text).unwrap();
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].dims, vec!["hour"]);
        assert_eq!(b[1].quota.as_ref().and_then(|q| q.get("tokensPerHour")).map(|b| b.consumed), Some(5.0));
        assert!(parse_batch("[]").is_err());
        assert!(parse_batch("<html>").unwrap_err().contains("not JSON"));
    }

    #[test]
    fn account_summaries_list_every_property_and_the_next_page() {
        let text = r#"{"accountSummaries":[
          {"name":"accountSummaries/1","account":"accounts/1","displayName":"Example Co","propertySummaries":[
            {"property":"properties/123","displayName":"Example Site","propertyType":"PROPERTY_TYPE_ORDINARY","parent":"accounts/1"},
            {"property":"properties/456","displayName":"Example Docs"}]},
          {"name":"accountSummaries/2","account":"accounts/2","displayName":"Empty Co"}
        ],"nextPageToken":"page-2"}"#;
        let (props, next) = parse_account_summaries(text).unwrap();
        assert_eq!(props.len(), 2);
        assert_eq!(props[0], PropertySummary { id: "123".into(), name: "Example Site".into(), account: "Example Co".into() });
        assert_eq!(props[1].id, "456");
        assert_eq!(next.as_deref(), Some("page-2"));
        let (none, last) = parse_account_summaries("{}").unwrap();
        assert!(none.is_empty() && last.is_none());
    }

    #[test]
    fn a_property_and_its_key_events_read() {
        let p = parse_property(r#"{"name":"properties/123","displayName":"Example Site","timeZone":"Europe/Berlin","currencyCode":"EUR"}"#).unwrap();
        assert_eq!(p, PropertyInfo { name: "Example Site".into(), time_zone: "Europe/Berlin".into(), currency: "EUR".into(), premium: false });
        assert!(parse_property(r#"{"serviceLevel":"GOOGLE_ANALYTICS_360"}"#).unwrap().premium);
        let (names, next) = parse_key_event_list(r#"{"keyEvents":[{"name":"properties/123/keyEvents/1","eventName":"purchase","createTime":"2026-01-01T00:00:00Z"},{"eventName":"sign_up"}]}"#).unwrap();
        assert_eq!(names, vec!["purchase", "sign_up"]);
        assert!(next.is_none());
        let (none, _) = parse_key_event_list("{}").unwrap();
        assert!(none.is_empty(), "a property with no key events lists none rather than failing");
    }

    #[test]
    fn samples_round_trip_and_skip_a_torn_line() {
        let s = Sample { t: 1_790_000_000.0, active: 71.0, views: 540.0, events: vec![("purchase".into(), 3.0), ("sign up".into(), 1.0)] };
        let line = format_sample(&s);
        assert_eq!(line, "1790000000 71 540 purchase=3 sign_up=1");
        let text = format!("{}\n1789999940 60 500\ngarbage\n1790000060 7", line);
        let back = parse_samples(&text);
        assert_eq!(back.len(), 2, "the torn last line and the garbage are skipped");
        assert_eq!(back[0].t, 1_789_999_940.0, "sorted by time");
        assert_eq!(back[1].events, vec![("purchase".to_string(), 3.0), ("sign_up".to_string(), 1.0)]);
        assert!(parse_samples("").is_empty());
        assert!(parse_samples("NaN 1 2\n1 inf 2").is_empty());
    }
}
