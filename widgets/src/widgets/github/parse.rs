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

//! Parsers for the GitHub widget's by-account figures, always compiled.
//!
//! Acquisition — which query to send, which cursor to follow — stays in
//! `main.rs`. These take text or counts and return values, so their tests
//! run on every CI target.

use chrono::{DateTime, Utc};

/// Twenty-four hours, the R24 bar. `[w]` only changes the sample of merged
/// PRs, never this threshold.
pub const FIRST_REVIEW_HOURS: f64 = 24.0;

/// Two days, the T2D bar. Draft time is inside `createdAt → mergedAt`.
pub const TIME_TO_MERGE_DAYS: f64 = 2.0;

/// Search returns at most this many nodes. Past it the set is incomplete
/// even if GitHub has more.
pub const SEARCH_NODE_CAP: i64 = 1000;

/// Of PRs that closed in the window, the share that merged.
///
/// `None` when nothing closed — the cell is `--`, not 0%.
pub fn parse_held(merged: i64, dropped: i64) -> Option<f64> {
    let closed = merged + dropped;
    if closed > 0 {
        Some(100.0 * merged as f64 / closed as f64)
    } else {
        None
    }
}

/// What a compact % cell should print.
///
/// Incomplete (a partial page, or the enricher still walking) is `···`.
/// An empty denominator is `--`. A sample is never a total.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PctCell {
    Incomplete,
    Empty,
    Value(f64),
}

/// One merged-in-window PR after the timing pass has read its nodes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MergedPr {
    pub id: String,
    pub created_at: String,
    pub merged_at: String,
    pub reviews: Vec<Review>,
    /// Reviews were truncated and no human was found on what we have.
    /// The first-review reading for this PR is unknown, not "none".
    pub reviews_incomplete: bool,
    pub reviews_cursor: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Review {
    pub submitted_at: String,
    pub author_login: String,
    pub author_type: String,
}

/// One page of a merged-PR search, as GraphQL returns it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MergedSearchPage {
    pub issue_count: Option<i64>,
    pub has_next_page: bool,
    pub end_cursor: String,
    pub prs: Vec<MergedPr>,
}

/// The landed-set numbers for one account's window.
#[derive(Clone, Debug, PartialEq)]
pub struct LandTiming {
    pub complete: bool,
    pub expected: i64,
    pub fetched: usize,
    pub r24: PctCell,
    pub t2d: PctCell,
    pub r24_count: i64,
    pub t2d_count: i64,
    pub no_human: Option<i64>,
    pub median_review_hours: Option<f64>,
    pub median_merge_days: Option<f64>,
}

impl Default for LandTiming {
    fn default() -> Self {
        Self {
            complete: false,
            expected: 0,
            fetched: 0,
            r24: PctCell::Incomplete,
            t2d: PctCell::Incomplete,
            r24_count: 0,
            t2d_count: 0,
            no_human: None,
            median_review_hours: None,
            median_merge_days: None,
        }
    }
}

/// CodeRabbit, Greptile and the rest land as ordinary reviews. Skipping
/// them is what keeps a bot-reviewed PR from looking instant.
pub fn parse_is_bot(login: &str, typename: &str) -> bool {
    typename.eq_ignore_ascii_case("Bot") || login.ends_with("[bot]")
}

/// Hours from `createdAt` to the first human review. `None` when nobody
/// human has looked, or when the reviews page was cut off before one.
pub fn parse_first_human_review_hours(created_at: &str, pr: &MergedPr) -> Option<f64> {
    if pr.reviews_incomplete && !pr.reviews.iter().any(|r| !parse_is_bot(&r.author_login, &r.author_type))
    {
        return None;
    }
    let created = parse_iso(created_at)?;
    let mut best: Option<f64> = None;
    for review in &pr.reviews {
        if parse_is_bot(&review.author_login, &review.author_type) {
            continue;
        }
        let Some(at) = parse_iso(&review.submitted_at) else {
            continue;
        };
        let hours = (at - created).num_seconds() as f64 / 3600.0;
        if hours < 0.0 {
            continue;
        }
        best = Some(best.map_or(hours, |h| h.min(hours)));
    }
    best
}

/// Days from `createdAt` to `mergedAt`. Draft time is inside the interval.
pub fn parse_time_to_merge_days(created_at: &str, merged_at: &str) -> Option<f64> {
    let created = parse_iso(created_at)?;
    let merged = parse_iso(merged_at)?;
    let days = (merged - created).num_seconds() as f64 / 86400.0;
    if days < 0.0 {
        return None;
    }
    Some(days)
}

pub fn parse_median(xs: &[f64]) -> Option<f64> {
    if xs.is_empty() {
        return None;
    }
    let mut s = xs.to_vec();
    s.sort_by(f64::total_cmp);
    let n = s.len();
    Some(if n % 2 == 1 {
        s[n / 2]
    } else {
        (s[n / 2 - 1] + s[n / 2]) / 2.0
    })
}

/// `expected` is `o0_merged`. If we fetched fewer nodes than that, the
/// percentages stay incomplete — a 100-node page is not a 247-PR window.
pub fn parse_land_timing(expected: i64, prs: &[MergedPr]) -> LandTiming {
    let fetched = prs.len();
    if expected <= 0 {
        return LandTiming {
            complete: true,
            expected: 0,
            fetched: 0,
            r24: PctCell::Empty,
            t2d: PctCell::Empty,
            r24_count: 0,
            t2d_count: 0,
            no_human: Some(0),
            median_review_hours: None,
            median_merge_days: None,
        };
    }
    let complete = (fetched as i64) >= expected && expected <= SEARCH_NODE_CAP;
    let mut r24_count = 0i64;
    let mut t2d_count = 0i64;
    let mut no_human = 0i64;
    let mut review_hours = Vec::new();
    let mut merge_days = Vec::new();
    let mut reviews_unknown = false;
    for pr in prs {
        if let Some(days) = parse_time_to_merge_days(&pr.created_at, &pr.merged_at) {
            merge_days.push(days);
            if days <= TIME_TO_MERGE_DAYS {
                t2d_count += 1;
            }
        }
        match parse_first_human_review_hours(&pr.created_at, pr) {
            Some(hours) => {
                review_hours.push(hours);
                if hours <= FIRST_REVIEW_HOURS {
                    r24_count += 1;
                }
            }
            None if pr.reviews_incomplete => reviews_unknown = true,
            None => no_human += 1,
        }
    }
    // A truncated review list on any PR means we cannot claim "N merged
    // with no human review" for the window, and R24 would undercount.
    let r24_ready = complete && !reviews_unknown;
    LandTiming {
        complete,
        expected,
        fetched,
        r24: parse_land_pct(r24_ready, expected, r24_count),
        t2d: parse_land_pct(complete, expected, t2d_count),
        r24_count,
        t2d_count,
        no_human: if r24_ready { Some(no_human) } else { None },
        median_review_hours: if r24_ready {
            parse_median(&review_hours)
        } else {
            None
        },
        median_merge_days: if complete {
            parse_median(&merge_days)
        } else {
            None
        },
    }
}

pub fn parse_land_pct(complete: bool, denom: i64, numer: i64) -> PctCell {
    if !complete {
        return PctCell::Incomplete;
    }
    if denom <= 0 {
        return PctCell::Empty;
    }
    PctCell::Value(100.0 * numer as f64 / denom as f64)
}

/// The compact cell: stale or incomplete is `···`, empty `--`, else `N%`.
pub fn parse_pct_text(stale: bool, cell: Option<PctCell>) -> String {
    if stale {
        return "···".into();
    }
    match cell {
        None | Some(PctCell::Incomplete) => "···".into(),
        Some(PctCell::Empty) => "--".into(),
        Some(PctCell::Value(v)) => format!("{:.0}%", v),
    }
}

/// One GraphQL search page of merged PRs, as text.
pub fn parse_merged_search_page(text: &str) -> Option<MergedSearchPage> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let search = if v["data"]["search"].is_object() {
        &v["data"]["search"]
    } else if v["search"].is_object() {
        &v["search"]
    } else {
        return None;
    };
    Some(MergedSearchPage {
        issue_count: search["issueCount"].as_i64(),
        has_next_page: search["pageInfo"]["hasNextPage"].as_bool().unwrap_or(false),
        end_cursor: search["pageInfo"]["endCursor"]
            .as_str()
            .unwrap_or("")
            .to_string(),
        prs: search["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(parse_merged_pr_value)
            .collect(),
    })
}

/// Extra review nodes for one PR, as a `reviews` connection or a `node`.
pub fn parse_review_page(text: &str) -> Option<(Vec<Review>, bool, String)> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let reviews = if v["data"]["node"]["reviews"].is_object() {
        &v["data"]["node"]["reviews"]
    } else if v["reviews"].is_object() {
        &v["reviews"]
    } else {
        return None;
    };
    Some((
        reviews["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(parse_review_value)
            .collect(),
        reviews["pageInfo"]["hasNextPage"].as_bool().unwrap_or(false),
        reviews["pageInfo"]["endCursor"]
            .as_str()
            .unwrap_or("")
            .to_string(),
    ))
}

fn parse_merged_pr_value(node: &serde_json::Value) -> Option<MergedPr> {
    // Search returns Issue | PullRequest. An issue node has no mergedAt.
    let merged_at = node["mergedAt"].as_str()?.to_string();
    let created_at = node["createdAt"].as_str()?.to_string();
    let reviews = node["reviews"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(parse_review_value)
        .collect::<Vec<_>>();
    let reviews_incomplete = node["reviews"]["pageInfo"]["hasNextPage"]
        .as_bool()
        .unwrap_or(false);
    Some(MergedPr {
        id: node["id"].as_str().unwrap_or("").to_string(),
        created_at,
        merged_at,
        reviews,
        reviews_incomplete,
        reviews_cursor: node["reviews"]["pageInfo"]["endCursor"]
            .as_str()
            .unwrap_or("")
            .to_string(),
    })
}

fn parse_review_value(node: &serde_json::Value) -> Option<Review> {
    Some(Review {
        submitted_at: node["submittedAt"].as_str()?.to_string(),
        author_login: node["author"]["login"].as_str().unwrap_or("").to_string(),
        author_type: node["author"]["__typename"]
            .as_str()
            .unwrap_or("")
            .to_string(),
    })
}

fn parse_iso(iso: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(iso)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn held_is_merged_among_closed() {
        let got = parse_held(15, 3).expect("closed PRs");
        assert!((got - 83.333).abs() < 0.01, "{got}");
        assert_eq!(parse_held(15, 0), Some(100.0));
        assert_eq!(parse_held(0, 3), Some(0.0));
        assert_eq!(parse_held(0, 0), None);
    }

    #[test]
    fn a_bot_review_is_not_a_human_one() {
        assert!(parse_is_bot("coderabbitai[bot]", "User"));
        assert!(parse_is_bot("copilot-pull-request-reviewer", "Bot"));
        assert!(parse_is_bot("greptile-apps[bot]", "Bot"));
        assert!(!parse_is_bot("ada", "User"));
        assert!(!parse_is_bot("ada", ""));
    }

    fn pr(created: &str, merged: &str, reviews: &[(&str, &str, &str)]) -> MergedPr {
        MergedPr {
            created_at: created.into(),
            merged_at: merged.into(),
            reviews: reviews
                .iter()
                .map(|(at, login, typ)| Review {
                    submitted_at: (*at).into(),
                    author_login: (*login).into(),
                    author_type: (*typ).into(),
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn first_human_review_skips_bots() {
        let merged = pr(
            "2026-09-01T10:00:00Z",
            "2026-09-02T10:00:00Z",
            &[
                ("2026-09-01T10:05:00Z", "coderabbitai[bot]", "Bot"),
                ("2026-09-01T12:00:00Z", "ada", "User"),
            ],
        );
        let hours = parse_first_human_review_hours(&merged.created_at, &merged).expect("ada");
        assert!((hours - 2.0).abs() < 0.01, "{hours}");
        assert!(hours <= FIRST_REVIEW_HOURS);
    }

    #[test]
    fn only_bots_is_not_a_first_review() {
        let merged = pr(
            "2026-09-01T10:00:00Z",
            "2026-09-01T11:00:00Z",
            &[("2026-09-01T10:01:00Z", "greptile-apps[bot]", "User")],
        );
        assert_eq!(
            parse_first_human_review_hours(&merged.created_at, &merged),
            None
        );
    }

    #[test]
    fn time_to_merge_includes_draft() {
        // Opened as a draft at 10:00, merged 36 hours later. The clock
        // starts at createdAt, not at ready-for-review.
        let days = parse_time_to_merge_days("2026-09-01T10:00:00Z", "2026-09-02T22:00:00Z")
            .expect("a duration");
        assert!((days - 1.5).abs() < 0.01, "{days}");
        assert!(days <= TIME_TO_MERGE_DAYS);
        let over = parse_time_to_merge_days("2026-09-01T10:00:00Z", "2026-09-04T10:00:00Z")
            .expect("three days");
        assert!((over - 3.0).abs() < 0.01, "{over}");
        assert!(over > TIME_TO_MERGE_DAYS);
    }

    #[test]
    fn the_bars_are_inclusive() {
        let at_24h = pr(
            "2026-09-01T10:00:00Z",
            "2026-09-04T10:00:00Z",
            &[("2026-09-02T10:00:00Z", "ada", "User")],
        );
        let hours = parse_first_human_review_hours(&at_24h.created_at, &at_24h).unwrap();
        assert!((hours - 24.0).abs() < 0.01);
        assert!(hours <= FIRST_REVIEW_HOURS);
        let at_2d = parse_time_to_merge_days("2026-09-01T10:00:00Z", "2026-09-03T10:00:00Z").unwrap();
        assert!((at_2d - 2.0).abs() < 0.01);
        assert!(at_2d <= TIME_TO_MERGE_DAYS);
    }

    #[test]
    fn r24_and_t2d_are_percent_of_merged() {
        // 15 merged, 3 dropped. R24 / T2D ignore the drops: a dropped PR
        // never lands, so it is not "slow to merge."
        let mut prs = Vec::new();
        for _ in 0..12 {
            prs.push(pr(
                "2026-09-01T10:00:00Z",
                "2026-09-01T20:00:00Z",
                &[("2026-09-01T12:00:00Z", "ada", "User")],
            ));
        }
        for _ in 0..3 {
            prs.push(pr(
                "2026-09-01T10:00:00Z",
                "2026-09-05T10:00:00Z",
                &[("2026-09-03T10:00:00Z", "ada", "User")],
            ));
        }
        let got = parse_land_timing(15, &prs);
        assert!(got.complete);
        assert_eq!(got.r24, PctCell::Value(80.0)); // 12 of 15, not 12 of 18
        assert_eq!(got.t2d, PctCell::Value(80.0));
        assert_eq!(got.r24_count, 12);
        assert_eq!(got.t2d_count, 12);
        assert_eq!(got.no_human, Some(0));
        assert_eq!(parse_held(15, 3).map(|v| v.round() as i64), Some(83));
    }

    #[test]
    fn a_partial_page_is_not_a_total() {
        let one = pr(
            "2026-09-01T10:00:00Z",
            "2026-09-01T12:00:00Z",
            &[("2026-09-01T10:30:00Z", "ada", "User")],
        );
        let sample: Vec<MergedPr> = (0..100).map(|_| one.clone()).collect();
        let got = parse_land_timing(247, &sample);
        assert!(!got.complete);
        assert_eq!(got.fetched, 100);
        assert_eq!(got.expected, 247);
        assert_eq!(got.r24, PctCell::Incomplete);
        assert_eq!(got.t2d, PctCell::Incomplete);
        assert_eq!(got.no_human, None);
        assert_eq!(got.median_review_hours, None);
        assert_eq!(got.median_merge_days, None);
        // The sample itself is 100% — that is what must not be drawn.
        assert_eq!(parse_land_pct(true, 100, 100), PctCell::Value(100.0));
        assert_eq!(parse_land_pct(false, 247, 100), PctCell::Incomplete);
    }

    #[test]
    fn past_the_search_cap_is_incomplete() {
        let one = pr("2026-09-01T10:00:00Z", "2026-09-01T12:00:00Z", &[]);
        let page: Vec<MergedPr> = (0..1000).map(|_| one.clone()).collect();
        let got = parse_land_timing(1000, &page);
        assert!(got.complete);
        let over = parse_land_timing(1001, &page);
        assert!(!over.complete);
        assert_eq!(over.r24, PctCell::Incomplete);
        assert_eq!(over.t2d, PctCell::Incomplete);
    }

    #[test]
    fn empty_merged_set_is_a_dash() {
        let got = parse_land_timing(0, &[]);
        assert!(got.complete);
        assert_eq!(got.r24, PctCell::Empty);
        assert_eq!(got.t2d, PctCell::Empty);
        assert_eq!(got.no_human, Some(0));
        assert_eq!(parse_pct_text(false, Some(PctCell::Empty)), "--");
        assert_eq!(parse_pct_text(false, Some(PctCell::Incomplete)), "···");
        assert_eq!(parse_pct_text(true, Some(PctCell::Value(71.0))), "···");
        assert_eq!(parse_pct_text(false, Some(PctCell::Value(71.0))), "71%");
        assert_eq!(parse_pct_text(false, None), "···");
    }

    #[test]
    fn a_search_page_reads_nodes_and_the_cursor() {
        let text = r#"{
          "data": {
            "search": {
              "issueCount": 2,
              "pageInfo": { "hasNextPage": true, "endCursor": "Y3Vyc29yOjEwMA==" },
              "nodes": [
                {
                  "id": "PR_1",
                  "createdAt": "2026-09-01T10:00:00Z",
                  "mergedAt": "2026-09-01T16:00:00Z",
                  "reviews": {
                    "pageInfo": { "hasNextPage": false, "endCursor": null },
                    "nodes": [
                      {
                        "submittedAt": "2026-09-01T10:10:00Z",
                        "author": { "__typename": "Bot", "login": "copilot-pull-request-reviewer" }
                      },
                      {
                        "submittedAt": "2026-09-01T11:00:00Z",
                        "author": { "__typename": "User", "login": "ada" }
                      }
                    ]
                  }
                },
                { "title": "an issue, not a PR" }
              ]
            }
          }
        }"#;
        let page = parse_merged_search_page(text).expect("a page");
        assert_eq!(page.issue_count, Some(2));
        assert!(page.has_next_page);
        assert_eq!(page.end_cursor, "Y3Vyc29yOjEwMA==");
        assert_eq!(page.prs.len(), 1);
        let hours = parse_first_human_review_hours(&page.prs[0].created_at, &page.prs[0]).unwrap();
        assert!((hours - 1.0).abs() < 0.01, "{hours}");
    }

    #[test]
    fn truncated_reviews_without_a_human_are_not_none() {
        let mut pr = pr(
            "2026-09-01T10:00:00Z",
            "2026-09-01T12:00:00Z",
            &[("2026-09-01T10:01:00Z", "coderabbitai[bot]", "Bot")],
        );
        pr.reviews_incomplete = true;
        assert_eq!(parse_first_human_review_hours(&pr.created_at, &pr), None);
        let got = parse_land_timing(1, &[pr]);
        // One PR, reviews cut off, no human seen: do not print 0% or
        // "1 with no human review".
        assert_eq!(got.r24, PctCell::Incomplete);
        assert_eq!(got.no_human, None);
        assert_eq!(got.t2d, PctCell::Value(100.0));
    }

    #[test]
    fn a_median_takes_the_middle() {
        assert_eq!(parse_median(&[]), None);
        assert_eq!(parse_median(&[6.0]), Some(6.0));
        assert_eq!(parse_median(&[1.0, 3.0]), Some(2.0));
        assert_eq!(parse_median(&[3.0, 1.0, 2.0]), Some(2.0));
    }
}
