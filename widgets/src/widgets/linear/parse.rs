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

//! What Linear's answer says about the hour's allowance.
//!
//! Linear refuses a request over its limit with HTTP 429, a `RATELIMITED`
//! code in the GraphQL errors, and a `Retry-After` header; every answer,
//! refused or not, carries `X-RateLimit-Requests-Remaining` and
//! `X-RateLimit-Requests-Reset`. Nothing backs off on these yet - they are
//! read so that a refusal can say which kind it was.

/// How long a refused request says to wait.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetryAfter {
    /// The header's usual form: whole seconds from now.
    Seconds(u64),
    /// The HTTP-date form, as Unix seconds.
    At(i64),
}

/// The rate-limit facts one answer carried. Every field is `None` or false
/// when the answer did not say, which is not the same as zero.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
// Read by the backoff OPS-286 adds; nothing on screen uses it yet.
#[allow(dead_code)]
pub struct RateLimit {
    /// The HTTP status of the answer.
    pub status: u16,
    /// A 429, or a `RATELIMITED` code in the body. Linear documents the
    /// first; the code is checked as well so a refusal on another status
    /// still reads as one.
    pub limited: bool,
    pub retry_after: Option<RetryAfter>,
    /// `X-RateLimit-Requests-Remaining`.
    pub remaining: Option<i64>,
    /// `X-RateLimit-Requests-Reset`, in Unix milliseconds as Linear sends it.
    pub reset_ms: Option<i64>,
}

/// `Retry-After`, in either of the forms HTTP allows.
pub fn parse_retry_after(text: &str) -> Option<RetryAfter> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    if t.chars().all(|c| c.is_ascii_digit()) {
        return t.parse().ok().map(RetryAfter::Seconds);
    }
    chrono::DateTime::parse_from_rfc2822(t)
        .ok()
        .map(|at| RetryAfter::At(at.timestamp()))
}

/// `X-RateLimit-Requests-Remaining`: requests left in this window.
pub fn parse_requests_remaining(text: &str) -> Option<i64> {
    text.trim().parse().ok().filter(|n: &i64| *n >= 0)
}

/// `X-RateLimit-Requests-Reset`: when the window resets, in Unix ms.
pub fn parse_requests_reset(text: &str) -> Option<i64> {
    text.trim().parse().ok().filter(|n: &i64| *n > 0)
}

/// Whether a GraphQL body names a `RATELIMITED` error.
pub fn parse_ratelimited(body: &str) -> bool {
    let Ok(data) = serde_json::from_str::<serde_json::Value>(body) else {
        return false;
    };
    data["errors"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|e| e["extensions"]["code"].as_str() == Some("RATELIMITED"))
}

/// Every rate-limit fact in one answer. `headers` are lower-cased names,
/// as core hands them back.
pub fn rate_limit(status: u16, headers: &[(String, String)], body: &str) -> RateLimit {
    let header = |name: &str| {
        headers
            .iter()
            .rev()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    };
    RateLimit {
        status,
        limited: status == 429 || parse_ratelimited(body),
        retry_after: header("retry-after").and_then(parse_retry_after),
        remaining: header("x-ratelimit-requests-remaining").and_then(parse_requests_remaining),
        reset_ms: header("x-ratelimit-requests-reset").and_then(parse_requests_reset),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_after_reads_seconds_and_dates() {
        assert_eq!(parse_retry_after("30"), Some(RetryAfter::Seconds(30)));
        assert_eq!(parse_retry_after(" 0 "), Some(RetryAfter::Seconds(0)));
        assert_eq!(
            parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT"),
            Some(RetryAfter::At(1_445_412_480))
        );
        assert_eq!(parse_retry_after(""), None);
        assert_eq!(parse_retry_after("-5"), None);
        assert_eq!(parse_retry_after("soon"), None);
    }

    #[test]
    fn remaining_and_reset_decline_what_is_not_a_count() {
        assert_eq!(parse_requests_remaining("2499"), Some(2499));
        assert_eq!(parse_requests_remaining("0"), Some(0));
        assert_eq!(parse_requests_remaining(""), None);
        assert_eq!(parse_requests_remaining("-1"), None);
        assert_eq!(parse_requests_remaining("lots"), None);
        assert_eq!(parse_requests_reset("1767225600000"), Some(1_767_225_600_000));
        assert_eq!(parse_requests_reset("0"), None);
        assert_eq!(parse_requests_reset("x"), None);
    }

    #[test]
    fn a_ratelimited_code_is_found_among_the_errors() {
        let body = r#"{"errors":[{"message":"x"},{"message":"slow down","extensions":{"code":"RATELIMITED"}}]}"#;
        assert!(parse_ratelimited(body));
        assert!(!parse_ratelimited(r#"{"errors":[{"message":"bad","extensions":{"code":"INVALID_INPUT"}}]}"#));
        assert!(!parse_ratelimited(r#"{"data":{}}"#));
        assert!(!parse_ratelimited("<html>gateway timeout</html>"));
    }

    #[test]
    fn a_refused_answer_says_what_kind_of_refusal_it_was() {
        let headers = vec![
            ("content-type".to_string(), "application/json".to_string()),
            ("retry-after".to_string(), "42".to_string()),
            ("x-ratelimit-requests-remaining".to_string(), "0".to_string()),
            ("x-ratelimit-requests-reset".to_string(), "1767225600000".to_string()),
        ];
        let got = rate_limit(429, &headers, "{}");
        assert_eq!(
            got,
            RateLimit {
                status: 429,
                limited: true,
                retry_after: Some(RetryAfter::Seconds(42)),
                remaining: Some(0),
                reset_ms: Some(1_767_225_600_000),
            }
        );
        // A refusal for another reason is not a rate limit, and headers it
        // did not send stay unknown rather than zero.
        let other = rate_limit(401, &[], r#"{"errors":[{"message":"no"}]}"#);
        assert!(!other.limited);
        assert_eq!(other.remaining, None);
        assert_eq!(other.retry_after, None);
        // The code alone is enough, whatever the status.
        let coded = rate_limit(
            400,
            &[],
            r#"{"errors":[{"message":"x","extensions":{"code":"RATELIMITED"}}]}"#,
        );
        assert!(coded.limited);
    }
}
