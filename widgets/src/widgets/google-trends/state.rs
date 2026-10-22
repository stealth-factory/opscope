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

//! What this widget has itself seen of one region's feed, across fetches.
//!
//! Google's feed says what is trending now and nothing about what was
//! trending before, so every figure here - SEEN, `new since`, LEFT THE
//! FEED - is this widget's own observation, counted only while it was
//! running. The file it keeps is that observation and nothing else; the
//! pane labels it as such rather than presenting it as Google's.
//!
//! Pure: no file is opened here, so every rule is tested on every target.

use std::collections::{BTreeMap, HashSet};

use crate::parse::{NewsItem, Trend};

/// How long a trend not seen at all is remembered, in seconds.
pub const FORGET_AFTER: f64 = 7.0 * 86_400.0;

/// How long a trend that left the feed stays in LEFT THE FEED, in seconds.
///
/// One fetch was too short: at the default five minutes a departure would
/// be on screen for one cycle and gone before anybody looked. An hour is
/// long enough to notice and short enough that the section stays a list
/// of recent departures rather than a history.
pub const LEFT_KEPT: f64 = 3_600.0;

/// One trend this widget has seen, by the run of fetches it was in.
#[derive(Clone, Debug, PartialEq)]
pub struct Seen {
    /// When the current unbroken run of fetches began.
    pub first_seen: f64,
    /// The last fetch it was in.
    pub last_seen: f64,
    /// Consecutive fetches it has been in, this run included.
    pub polls: u32,
    /// Its bucket the last time it was seen, for after it leaves.
    pub traffic: String,
}

/// A trend that was in the previous fetch and is not in this one.
#[derive(Clone, Debug, PartialEq)]
pub struct Left {
    pub title: String,
    pub traffic: String,
    pub first_seen: f64,
    pub last_seen: f64,
    /// The fetch that found it gone.
    pub left_at: f64,
    pub polls: u32,
    /// Its news, while this process still has it. Not saved: a restart
    /// keeps the fact that it left and not the stories, so `None` is
    /// "not kept", which is not the same as Google listing none.
    pub news: Option<Vec<NewsItem>>,
}

/// What one fetch looked like against the ones before it.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Observation {
    /// Per item, in feed order: consecutive fetches it has been in.
    pub seen: Vec<u32>,
    /// Items whose run began with this fetch.
    pub new_count: usize,
    /// The fetch this one was compared with, or `None` on a first check.
    pub since: Option<f64>,
    /// A previous fetch too old to compare with, when that is why this
    /// is a first check.
    pub stale: Option<f64>,
}

/// One region's memory.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Memory {
    /// The last fetch that returned a feed, empty or not.
    pub previous_fetch: Option<f64>,
    /// The titles that fetch held.
    pub previous: Vec<String>,
    pub trends: BTreeMap<String, Seen>,
    pub left: Vec<Left>,
    /// News of the previous fetch's items, in memory only, so a trend that
    /// leaves can still be opened.
    pub news: BTreeMap<String, Vec<NewsItem>>,
}

impl Memory {
    /// Fold one successful fetch in, and say how it compares.
    ///
    /// A trend counts as continuing only if it was in the fetch directly
    /// before this one: SEEN is consecutive fetches, so a term that drops
    /// out and comes back starts again at one and is `new` again.
    ///
    /// A previous fetch more than `max_gap` seconds old is not compared
    /// with: the widget was not watching in between, so it cannot say a
    /// trend ran on through the gap or when one left. That fetch is
    /// reported as `stale` and this one starts afresh, as a first check.
    pub fn observe(&mut self, items: &[Trend], now: f64, max_gap: f64) -> Observation {
        let since = self.previous_fetch.filter(|&at| now - at <= max_gap);
        let stale = self.previous_fetch.filter(|_| since.is_none());
        let before: HashSet<&str> = if since.is_some() {
            self.previous.iter().map(String::as_str).collect()
        } else {
            HashSet::new()
        };
        let mut seen = Vec::with_capacity(items.len());
        let mut new_count = 0usize;
        let mut counted: HashSet<String> = HashSet::new();
        for item in items {
            // Google has never listed one term twice, but a term listed
            // twice must not be counted as two fetches.
            if !counted.insert(item.title.clone()) {
                seen.push(self.trends.get(&item.title).map_or(1, |s| s.polls));
                continue;
            }
            let carried = before.contains(item.title.as_str());
            let entry = self.trends.get_mut(&item.title).filter(|_| carried);
            let polls = match entry {
                Some(s) => {
                    s.polls += 1;
                    s.last_seen = now;
                    s.traffic = item.traffic.clone();
                    s.polls
                }
                None => {
                    self.trends.insert(
                        item.title.clone(),
                        Seen {
                            first_seen: now,
                            last_seen: now,
                            polls: 1,
                            traffic: item.traffic.clone(),
                        },
                    );
                    1
                }
            };
            if polls == 1 {
                new_count += 1;
            }
            seen.push(polls);
        }
        // Back in the feed is not "left the feed" any more.
        self.left.retain(|l| !counted.contains(&l.title));
        if since.is_some() {
            for title in &self.previous {
                if counted.contains(title) || self.left.iter().any(|l| &l.title == title) {
                    continue;
                }
                let Some(s) = self.trends.get(title) else {
                    continue;
                };
                self.left.push(Left {
                    title: title.clone(),
                    traffic: s.traffic.clone(),
                    first_seen: s.first_seen,
                    last_seen: s.last_seen,
                    left_at: now,
                    polls: s.polls,
                    news: self.news.get(title).cloned(),
                });
            }
        }
        self.previous = items.iter().map(|i| i.title.clone()).collect();
        self.previous.dedup();
        self.news = items
            .iter()
            .map(|i| (i.title.clone(), i.news.clone()))
            .collect();
        self.previous_fetch = Some(now);
        self.prune(now);
        Observation {
            seen,
            new_count: if since.is_some() { new_count } else { 0 },
            since,
            stale,
        }
    }

    /// Drop what is too old to say anything about.
    pub fn prune(&mut self, now: f64) {
        let current: HashSet<&String> = self.previous.iter().collect();
        self.trends
            .retain(|title, s| current.contains(title) || now - s.last_seen <= FORGET_AFTER);
        self.left.retain(|l| now - l.left_at <= LEFT_KEPT);
    }

    /// The memory as the JSON written to disk.
    pub fn to_json(&self) -> String {
        let trends: serde_json::Map<String, serde_json::Value> = self
            .trends
            .iter()
            .map(|(title, s)| {
                (
                    title.clone(),
                    serde_json::json!({
                        "first_seen": s.first_seen,
                        "last_seen": s.last_seen,
                        "polls": s.polls,
                        "traffic": s.traffic,
                    }),
                )
            })
            .collect();
        let left: Vec<serde_json::Value> = self
            .left
            .iter()
            .map(|l| {
                serde_json::json!({
                    "title": l.title,
                    "traffic": l.traffic,
                    "first_seen": l.first_seen,
                    "last_seen": l.last_seen,
                    "left_at": l.left_at,
                    "polls": l.polls,
                })
            })
            .collect();
        let value = serde_json::json!({
            "version": 1,
            "previous_fetch": self.previous_fetch,
            "previous": self.previous,
            "trends": trends,
            "left": left,
        });
        serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".into())
    }
}

/// A saved memory, or why it cannot be used.
///
/// Strict on shape: a file that parses as JSON but is not this shape is as
/// unusable as one that does not parse, and half-reading it would count
/// polls against a history that was never there.
pub fn parse_state(text: &str) -> Result<Memory, String> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("not JSON ({e})"))?;
    let obj = value.as_object().ok_or("not a JSON object")?;
    if obj.get("version").and_then(|v| v.as_u64()) != Some(1) {
        return Err("unknown version".into());
    }
    let num = |v: &serde_json::Value, key: &str| -> Result<f64, String> {
        v.get(key)
            .and_then(|n| n.as_f64())
            .ok_or_else(|| format!("{key} is missing or not a number"))
    };
    let previous_fetch = match obj.get("previous_fetch") {
        None | Some(serde_json::Value::Null) => None,
        Some(v) => Some(v.as_f64().ok_or("previous_fetch is not a number")?),
    };
    let previous = obj
        .get("previous")
        .and_then(|v| v.as_array())
        .ok_or("previous is missing")?
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_string)
                .ok_or("previous holds a non-string")
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut trends = BTreeMap::new();
    for (title, s) in obj
        .get("trends")
        .and_then(|v| v.as_object())
        .ok_or("trends is missing")?
    {
        trends.insert(
            title.clone(),
            Seen {
                first_seen: num(s, "first_seen")?,
                last_seen: num(s, "last_seen")?,
                polls: s
                    .get("polls")
                    .and_then(|n| n.as_u64())
                    .ok_or("polls is missing")? as u32,
                traffic: s
                    .get("traffic")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string(),
            },
        );
    }
    let mut left = Vec::new();
    for l in obj
        .get("left")
        .and_then(|v| v.as_array())
        .ok_or("left is missing")?
    {
        left.push(Left {
            title: l
                .get("title")
                .and_then(|t| t.as_str())
                .ok_or("a left entry has no title")?
                .to_string(),
            traffic: l
                .get("traffic")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string(),
            first_seen: num(l, "first_seen")?,
            last_seen: num(l, "last_seen")?,
            left_at: num(l, "left_at")?,
            polls: l.get("polls").and_then(|n| n.as_u64()).unwrap_or(1) as u32,
            news: None,
        });
    }
    Ok(Memory {
        previous_fetch,
        previous,
        trends,
        left,
        news: BTreeMap::new(),
    })
}

/// A region code as a file name: letters, digits, `-` and `_` only.
pub fn file_stem(geo: &str) -> String {
    let stem: String = geo
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if stem.is_empty() { "_".into() } else { stem }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wider than every step these tests take between fetches.
    const GAP: f64 = 900.0;

    fn trend(title: &str, traffic: &str) -> Trend {
        Trend {
            title: title.into(),
            traffic: traffic.into(),
            news: vec![NewsItem {
                title: format!("{title} story"),
                url: "https://example.com/s".into(),
                source: "Example News".into(),
            }],
            ..Trend::default()
        }
    }

    #[test]
    fn the_first_check_has_nothing_to_compare_with() {
        let mut m = Memory::default();
        let o = m.observe(&[trend("alpha", "200+"), trend("beta", "500+")], 1000.0, GAP);
        assert_eq!(o.seen, vec![1, 1]);
        assert_eq!(o.since, None);
        assert_eq!(o.new_count, 0, "a first check is not a count of new trends");
        assert!(m.left.is_empty());
    }

    #[test]
    fn seen_counts_consecutive_fetches_and_new_ones_start_at_one() {
        let mut m = Memory::default();
        m.observe(&[trend("alpha", "200+"), trend("beta", "500+")], 1000.0, GAP);
        let o = m.observe(&[trend("alpha", "1000+"), trend("gamma", "200+")], 1300.0, GAP);
        assert_eq!(o.seen, vec![2, 1]);
        assert_eq!(o.new_count, 1);
        assert_eq!(o.since, Some(1000.0));
        assert_eq!(m.trends["alpha"].traffic, "1000+");
        assert_eq!(m.trends["alpha"].first_seen, 1000.0);
        let o = m.observe(&[trend("alpha", "1000+"), trend("gamma", "200+")], 1600.0, GAP);
        assert_eq!(o.seen, vec![3, 2]);
        assert_eq!(o.new_count, 0);
    }

    #[test]
    fn a_trend_missing_from_this_fetch_has_left_the_feed() {
        let mut m = Memory::default();
        m.observe(&[trend("alpha", "200+"), trend("beta", "500+")], 1000.0, GAP);
        m.observe(&[trend("alpha", "200+"), trend("beta", "500+")], 1300.0, GAP);
        m.observe(&[trend("alpha", "200+")], 1600.0, GAP);
        assert_eq!(m.left.len(), 1);
        let gone = &m.left[0];
        assert_eq!(gone.title, "beta");
        assert_eq!(gone.traffic, "500+");
        assert_eq!(gone.polls, 2);
        assert_eq!(gone.first_seen, 1000.0);
        assert_eq!(gone.last_seen, 1300.0);
        assert_eq!(gone.left_at, 1600.0);
        assert_eq!(
            gone.news.as_ref().map(Vec::len),
            Some(1),
            "its stories are kept while the process runs"
        );
        // Still gone on the next fetch: listed once, not twice.
        m.observe(&[trend("alpha", "200+")], 1900.0, GAP);
        assert_eq!(m.left.len(), 1);
    }

    #[test]
    fn a_trend_that_comes_back_is_new_again_and_no_longer_left() {
        let mut m = Memory::default();
        m.observe(&[trend("alpha", "200+"), trend("beta", "500+")], 1000.0, GAP);
        m.observe(&[trend("alpha", "200+")], 1300.0, GAP);
        assert_eq!(m.left.len(), 1);
        let o = m.observe(&[trend("alpha", "200+"), trend("beta", "500+")], 1600.0, GAP);
        assert_eq!(o.seen, vec![3, 1]);
        assert_eq!(o.new_count, 1);
        assert!(m.left.is_empty());
        assert_eq!(m.trends["beta"].first_seen, 1600.0);
    }

    #[test]
    fn departures_are_kept_an_hour_and_old_trends_a_week() {
        let mut m = Memory::default();
        m.observe(&[trend("alpha", "200+"), trend("beta", "500+")], 1000.0, GAP);
        m.observe(&[trend("alpha", "200+")], 1300.0, GAP);
        m.observe(&[trend("alpha", "200+")], 1300.0 + LEFT_KEPT - 1.0, f64::INFINITY);
        assert_eq!(m.left.len(), 1);
        m.observe(&[trend("alpha", "200+")], 1300.0 + LEFT_KEPT + 1.0, f64::INFINITY);
        assert!(m.left.is_empty());
        assert!(m.trends.contains_key("beta"), "a week has not passed");
        m.observe(&[trend("alpha", "200+")], 1000.0 + FORGET_AFTER + 1.0, f64::INFINITY);
        assert!(!m.trends.contains_key("beta"));
        assert!(
            m.trends.contains_key("alpha"),
            "a trend still in the feed is never pruned"
        );
    }

    #[test]
    fn a_previous_fetch_too_old_to_compare_is_a_fresh_start() {
        let mut m = Memory::default();
        m.observe(&[trend("alpha", "200+"), trend("beta", "500+")], 1000.0, GAP);
        // Hours later, as after a restart: beta is absent, alpha is back.
        let later = 1000.0 + 6.0 * 3_600.0;
        let o = m.observe(&[trend("alpha", "200+")], later, GAP);
        assert_eq!(o.since, None, "nothing was watched in between");
        assert_eq!(o.stale, Some(1000.0));
        assert_eq!(o.seen, vec![1], "a run does not carry across the gap");
        assert_eq!(o.new_count, 0);
        assert!(m.left.is_empty(), "beta left at some unknown time, not now");
        // And the fetch after that compares normally again.
        let o = m.observe(&[trend("alpha", "200+")], later + 300.0, GAP);
        assert_eq!(o.since, Some(later));
        assert_eq!(o.stale, None);
        assert_eq!(o.seen, vec![2]);
    }

    #[test]
    fn an_empty_fetch_counts_as_a_fetch() {
        let mut m = Memory::default();
        m.observe(&[trend("alpha", "200+")], 1000.0, GAP);
        let o = m.observe(&[], 1300.0, GAP);
        assert!(o.seen.is_empty());
        assert_eq!(m.left.len(), 1);
        assert_eq!(m.previous_fetch, Some(1300.0));
    }

    #[test]
    fn a_term_listed_twice_is_one_fetch() {
        let mut m = Memory::default();
        let o = m.observe(&[trend("alpha", "200+"), trend("alpha", "200+")], 1000.0, GAP);
        assert_eq!(o.seen, vec![1, 1]);
        let o = m.observe(&[trend("alpha", "200+"), trend("alpha", "200+")], 1300.0, GAP);
        assert_eq!(o.seen, vec![2, 2]);
    }

    #[test]
    fn the_saved_memory_reads_back() {
        let mut m = Memory::default();
        m.observe(
            &[trend("alpha", "200+"), trend("beta \"q\"", "500+")],
            1000.0,
            GAP,
        );
        m.observe(&[trend("alpha", "200+")], 1300.0, GAP);
        let back = parse_state(&m.to_json()).unwrap();
        assert_eq!(back.previous_fetch, m.previous_fetch);
        assert_eq!(back.previous, m.previous);
        assert_eq!(back.trends, m.trends);
        assert_eq!(back.left.len(), 1);
        assert_eq!(back.left[0].title, "beta \"q\"");
        assert_eq!(back.left[0].news, None, "stories are not saved");
        // And it carries on counting from where it was.
        let mut back = back;
        let o = back.observe(&[trend("alpha", "200+")], 1600.0, GAP);
        assert_eq!(o.seen, vec![3]);
    }

    #[test]
    fn a_corrupt_file_is_an_error_not_a_panic() {
        assert!(parse_state("").is_err());
        assert!(
            parse_state("{not json")
                .unwrap_err()
                .starts_with("not JSON")
        );
        assert!(parse_state("[]").is_err());
        assert!(parse_state(r#"{"version":2}"#).is_err());
        assert!(parse_state(r#"{"version":1,"previous":[],"trends":{"a":{}},"left":[]}"#).is_err());
        assert!(parse_state(r#"{"version":1,"previous":[1],"trends":{},"left":[]}"#).is_err());
        let ok = parse_state(
            r#"{"version":1,"previous_fetch":null,"previous":[],"trends":{},"left":[]}"#,
        );
        assert_eq!(ok.unwrap(), Memory::default());
    }

    #[test]
    fn a_geo_becomes_a_safe_file_name() {
        assert_eq!(file_stem("US"), "US");
        assert_eq!(file_stem("../etc"), "etc");
        assert_eq!(file_stem("/"), "_");
    }
}
