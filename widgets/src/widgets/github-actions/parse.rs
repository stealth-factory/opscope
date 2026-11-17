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

//! Parsers for what GitHub says about its rate limits, always compiled.
//!
//! They take the response's header block as text, so they compile on every
//! target and their fixture tests run wherever `cargo test` does.
#![allow(dead_code)]

/// GitHub's rate-limit headers off one response, each one only if it was
/// there and read as a number. Nothing is filled in: a missing reset is
/// `None`, not "now".
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RateHeaders {
    pub remaining: Option<i64>,
    pub limit: Option<i64>,
    /// When the bucket refills, in epoch seconds.
    pub reset: Option<i64>,
    /// Which bucket answered: `core`, `graphql`, `search` and so on. REST
    /// and GraphQL are counted apart, so a reset means nothing without it.
    pub resource: Option<String>,
    /// Seconds to wait, sent with a secondary limit. Only the delta-seconds
    /// form is read; GitHub does not send the HTTP-date form.
    pub retry_after: Option<i64>,
}

/// What a response says about asking again.
#[derive(Clone, Debug, PartialEq)]
pub enum RateLimit {
    /// Ask whenever.
    Ok,
    /// The primary budget for `resource` is spent until `until` (epoch
    /// seconds, GitHub's own reset).
    Spent { until: i64, resource: Option<String> },
    /// A secondary limit: 403 or 429 with `retry-after`, or a bare 429.
    /// Stop until `until`.
    Secondary { until: i64 },
}

impl RateLimit {
    /// When asking is allowed again, or `None` when it already is.
    pub fn until(&self) -> Option<i64> {
        match self {
            RateLimit::Ok => None,
            RateLimit::Spent { until, .. } | RateLimit::Secondary { until } => Some(*until),
        }
    }
}

/// GitHub's advice for a secondary limit that names no wait, and for a
/// spent budget whose reset header did not arrive: wait at least a minute.
pub const MIN_WAIT: i64 = 60;

/// Read the rate-limit headers out of a header block, one `name: value` per
/// line. Names are matched case-insensitively; a status line or any other
/// header is skipped.
pub fn parse_rate_headers(head: &str) -> RateHeaders {
    let mut out = RateHeaders::default();
    for line in head.lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match name.trim().to_ascii_lowercase().as_str() {
            "x-ratelimit-remaining" => out.remaining = value.parse().ok(),
            "x-ratelimit-limit" => out.limit = value.parse().ok(),
            "x-ratelimit-reset" => out.reset = value.parse().ok(),
            "x-ratelimit-resource" if !value.is_empty() => out.resource = Some(value.to_string()),
            "retry-after" => out.retry_after = value.parse().ok().filter(|s: &i64| *s >= 0),
            _ => {}
        }
    }
    out
}

/// Classify one response by its status and rate-limit headers, `now` in
/// epoch seconds.
///
/// A `retry-after` on a 403 or 429 is a secondary limit and wins, because
/// it is the wait GitHub asked for. Otherwise a `remaining` of zero is the
/// primary budget spent, on any status: a 200 that used the last request
/// means the next one will be refused. A 403 with neither is an ordinary
/// refusal (a missing scope, a private repo), not a limit. A 429 with
/// neither is still a limit, and waits the minute GitHub advises.
pub fn classify_rate(status: u16, h: &RateHeaders, now: i64) -> RateLimit {
    let refused = status == 403 || status == 429;
    if refused {
        if let Some(wait) = h.retry_after {
            // The header is any non-negative integer. A plain add wraps a
            // wait near `i64::MAX` into a deadline already in the past.
            return RateLimit::Secondary { until: now.saturating_add(wait) };
        }
    }
    if h.remaining == Some(0) {
        return RateLimit::Spent {
            until: h.reset.unwrap_or(now + MIN_WAIT),
            resource: h.resource.clone(),
        };
    }
    if status == 429 {
        return RateLimit::Secondary { until: now + MIN_WAIT };
    }
    RateLimit::Ok
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEAD: &str = "HTTP/2 200\r\n\
        content-type: application/json\r\n\
        X-RateLimit-Limit: 5000\r\n\
        X-RateLimit-Remaining: 4840\r\n\
        X-RateLimit-Reset: 1790000000\r\n\
        X-RateLimit-Used: 160\r\n\
        X-RateLimit-Resource: core\r\n";

    #[test]
    fn rate_headers_are_read_whatever_their_case() {
        let got = parse_rate_headers(HEAD);
        assert_eq!(got.remaining, Some(4840));
        assert_eq!(got.limit, Some(5000));
        assert_eq!(got.reset, Some(1_790_000_000));
        assert_eq!(got.resource.as_deref(), Some("core"));
        assert_eq!(got.retry_after, None);
    }

    #[test]
    fn a_missing_or_garbled_header_is_absent_not_zero() {
        let got = parse_rate_headers("x-ratelimit-remaining: lots\nretry-after: soon\n");
        assert_eq!(got, RateHeaders::default());
        assert_eq!(parse_rate_headers(""), RateHeaders::default());
    }

    #[test]
    fn a_healthy_answer_is_ok() {
        assert_eq!(classify_rate(200, &parse_rate_headers(HEAD), 100), RateLimit::Ok);
        assert_eq!(RateLimit::Ok.until(), None);
    }

    #[test]
    fn a_spent_budget_waits_for_githubs_own_reset() {
        let h = parse_rate_headers(
            "x-ratelimit-remaining: 0\nx-ratelimit-limit: 5000\n\
             x-ratelimit-reset: 1790000000\nx-ratelimit-resource: graphql\n",
        );
        let want = RateLimit::Spent { until: 1_790_000_000, resource: Some("graphql".into()) };
        // The answer that spent the last request still came back 200.
        assert_eq!(classify_rate(200, &h, 1_789_999_000), want);
        assert_eq!(classify_rate(403, &h, 1_789_999_000), want);
        assert_eq!(want.until(), Some(1_790_000_000));
    }

    #[test]
    fn a_secondary_limit_waits_for_retry_after_from_now() {
        let h = parse_rate_headers("retry-after: 30\nx-ratelimit-remaining: 4000\n");
        assert_eq!(classify_rate(403, &h, 1000), RateLimit::Secondary { until: 1030 });
        assert_eq!(classify_rate(429, &h, 1000), RateLimit::Secondary { until: 1030 });
        // retry-after beats a spent budget: it is the wait GitHub asked for.
        let both = parse_rate_headers("retry-after: 5\nx-ratelimit-remaining: 0\nx-ratelimit-reset: 9999\n");
        assert_eq!(classify_rate(429, &both, 1000), RateLimit::Secondary { until: 1005 });
    }

    #[test]
    fn a_plain_403_is_a_refusal_not_a_limit() {
        let h = parse_rate_headers("x-ratelimit-remaining: 4000\nx-ratelimit-limit: 5000\n");
        assert_eq!(classify_rate(403, &h, 1000), RateLimit::Ok);
        assert_eq!(classify_rate(404, &RateHeaders::default(), 1000), RateLimit::Ok);
    }

    #[test]
    fn a_bare_429_waits_the_minute_github_advises() {
        assert_eq!(
            classify_rate(429, &RateHeaders::default(), 1000),
            RateLimit::Secondary { until: 1000 + MIN_WAIT }
        );
    }

    #[test]
    fn retry_after_on_a_200_is_not_a_limit() {
        let h = parse_rate_headers("retry-after: 30\n");
        assert_eq!(classify_rate(200, &h, 1000), RateLimit::Ok);
    }

    #[test]
    fn a_huge_retry_after_saturates_the_deadline_instead_of_wrapping() {
        let h = parse_rate_headers("retry-after: 9223372036854775807\n");
        assert_eq!(h.retry_after, Some(i64::MAX));
        let now = 1_700_000_000;
        assert_eq!(classify_rate(429, &h, now), RateLimit::Secondary { until: i64::MAX });
        assert_eq!(classify_rate(403, &h, now), RateLimit::Secondary { until: i64::MAX });
    }
}
