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

//! Parsers for bodies the vendors send. Always compiled, whatever the host.

use crate::iso_epoch;

/// Reset credits still usable on a Codex account.
///
/// `left` is a count the body supports. `expiries` is one entry per credit
/// that count was taken from, soonest first. `Some` is that credit's
/// `expires_at`. `None` means the date could not be read, so the pane says
/// it is unknown rather than inventing one. An empty `expiries` is a count
/// the server stated without listing the credits, so no date row is drawn.
#[derive(Clone, Debug, PartialEq)]
pub struct ResetBank {
    pub left: u64,
    pub expiries: Vec<Option<f64>>,
}

/// Codex `GET /wham/rate-limit-reset-credits`.
///
/// The body is a `credits` list plus `available_count`, or a count with no
/// list. A count that arrived on some other payload is not this body.
///
/// A credit is still in the bank when its status is `available` and its
/// expiry is either unreadable or still ahead of `now`. A spent credit, an
/// unknown status, or a credit already past its expiry is not part of
/// `left`. `now` is an argument so the same body parses the same way in a
/// test as on the pane.
///
/// An `expires_at` that is missing, null, or not a time stays in the bank
/// with no date. A credit that is not an object cannot be classified, so
/// the server's `available_count` is used on its own, with no dates, and
/// only when it is a non-negative whole number. A negative count makes the
/// body unusable.
pub fn parse_codex_reset_credits(text: &str, now: f64) -> Option<ResetBank> {
    let body: serde_json::Value = serde_json::from_str(text).ok()?;
    let obj = body.as_object()?;
    let reported = match obj.get("available_count") {
        None => None,
        Some(value) => Some(whole_count(value)?),
    };
    // Null is a count with no list, same as the key being absent. An
    // unreadable list falls through to the count alone below.
    let Some(credits) = obj.get("credits").filter(|value| !value.is_null()) else {
        return Some(ResetBank {
            left: reported?,
            expiries: Vec::new(),
        });
    };
    let Some(credits) = credits.as_array() else {
        return count_only(reported);
    };
    // An empty list published a count and no credits to classify. The
    // count is the bank. Zero, when that is the count, is a real empty bank.
    if credits.is_empty() {
        return Some(ResetBank {
            left: reported.unwrap_or(0),
            expiries: Vec::new(),
        });
    }
    let mut expiries = Vec::new();
    for credit in credits {
        let Some(credit) = credit.as_object() else {
            return count_only(reported);
        };
        if credit.get("status").and_then(|v| v.as_str()) != Some("available") {
            continue;
        }
        // Missing, null, a non-string, or a string that is not a time is
        // still this credit. The date is unknown. It is not dropped, and
        // it is not given an invented expiry.
        let expiry = match credit.get("expires_at") {
            Some(serde_json::Value::String(raw)) => iso_epoch(raw),
            _ => None,
        };
        if expiry.is_some_and(|at| at <= now) {
            continue;
        }
        expiries.push(expiry);
    }
    expiries.sort_by(|a, b| match (a, b) {
        (Some(left), Some(right)) => left.total_cmp(right),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    // The list was readable, so the bank is the credits still in it. A
    // reported count that disagrees is not drawn beside them: the lines
    // under the number have to be that number.
    Some(ResetBank {
        left: expiries.len() as u64,
        expiries,
    })
}

/// The count alone, once the list can no longer be trusted credit by credit.
fn count_only(reported: Option<u64>) -> Option<ResetBank> {
    Some(ResetBank {
        left: reported?,
        expiries: Vec::new(),
    })
}

fn whole_count(value: &serde_json::Value) -> Option<u64> {
    if let Some(n) = value.as_u64() {
        return Some(n);
    }
    value.as_i64().filter(|n| *n >= 0).map(|n| n as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(iso: &str) -> f64 {
        iso_epoch(iso).expect(iso)
    }

    #[test]
    fn still_usable_credits_are_counted_soonest_first() {
        let now = at("2026-07-01T00:00:00Z");
        let body = r#"{
            "available_count": 9,
            "credits": [
                {"status":"available","reset_type":"codex_rate_limits",
                 "expires_at":"2026-06-17T00:39:53Z"},
                {"status":"available","reset_type":"codex_rate_limits",
                 "expires_at":"2026-07-18T00:39:53.731630Z"},
                {"status":"available","reset_type":"codex_rate_limits",
                 "expires_at":"2026-07-12T04:03:43.263391Z"},
                {"status":"redeemed","expires_at":"2026-08-01T00:00:00Z"},
                {"status":"available","expires_at":null},
                {"status":"future_status","expires_at":"2026-07-10T04:03:43Z"}
            ]
        }"#;
        let bank = parse_codex_reset_credits(body, now).expect("a readable inventory");
        // The reported 9 is not the bank: one credit has already expired,
        // one is redeemed, and one has a status this pane does not know.
        // The two that remain, plus the one whose date could not be read,
        // are what is left.
        assert_eq!(bank.left, 3);
        assert_eq!(bank.expiries, vec![
            Some(at("2026-07-12T04:03:43.263391Z")),
            Some(at("2026-07-18T00:39:53.731630Z")),
            None,
        ]);
    }

    #[test]
    fn a_credit_expiring_at_this_instant_is_no_longer_in_the_bank() {
        let now = at("2026-07-01T00:00:00Z");
        let body = r#"{
            "available_count": 1,
            "credits": [{"status":"available","expires_at":"2026-07-01T00:00:00Z"}]
        }"#;
        let bank = parse_codex_reset_credits(body, now).expect("the list was readable");
        assert_eq!(bank.left, 0);
        assert!(bank.expiries.is_empty());
    }

    #[test]
    fn a_null_credit_list_keeps_the_count_and_invents_no_expiry() {
        let bank = parse_codex_reset_credits(r#"{"available_count":2,"credits":null}"#, 0.0)
            .expect("the count");
        assert_eq!(bank.left, 2);
        assert!(bank.expiries.is_empty(), "null is not a list of expiries");
        assert!(parse_codex_reset_credits(r#"{"credits":null}"#, 0.0).is_none());
    }

    #[test]
    fn an_empty_list_keeps_the_count_the_server_stated() {
        let now = at("2026-07-01T00:00:00Z");
        let none = parse_codex_reset_credits(r#"{"credits":[],"available_count":0}"#, now)
            .expect("a real zero");
        assert_eq!(none.left, 0);
        assert!(none.expiries.is_empty());
        let stated = parse_codex_reset_credits(r#"{"credits":[],"available_count":2}"#, now)
            .expect("a count without the credits listed");
        assert_eq!(stated.left, 2);
        assert!(
            stated.expiries.is_empty(),
            "no expiry was sent, so none is drawn"
        );
    }

    #[test]
    fn a_summary_with_only_a_count_is_a_bank_without_expiries() {
        let bank = parse_codex_reset_credits(r#"{"available_count":4}"#, 0.0).expect("a summary");
        assert_eq!(bank.left, 4);
        assert!(bank.expiries.is_empty());
    }

    #[test]
    fn a_negative_count_is_not_a_bank() {
        assert!(
            parse_codex_reset_credits(
                r#"{"credits":[{"status":"available","expires_at":null}],"available_count":-1}"#,
                0.0
            )
            .is_none()
        );
        assert!(parse_codex_reset_credits(r#"{"available_count":2.5}"#, 0.0).is_none());
        assert!(parse_codex_reset_credits("not json", 0.0).is_none());
        assert!(parse_codex_reset_credits("{}", 0.0).is_none());
    }

    #[test]
    fn an_expiry_that_cannot_be_read_still_counts_and_keeps_the_other_dates() {
        let now = at("2026-07-01T00:00:00Z");
        let bank = parse_codex_reset_credits(
            r#"{"available_count":2,"credits":[
                {"status":"available","expires_at":"2026-07-12T00:00:00Z"},
                {"status":"available","expires_at":"not-a-time"}
            ]}"#,
            now,
        )
        .expect("both credits stay in the bank");
        assert_eq!(bank.left, 2);
        assert_eq!(bank.expiries, vec![Some(at("2026-07-12T00:00:00Z")), None]);
        let only = parse_codex_reset_credits(
            r#"{"credits":[{"status":"available","expires_at":"not-a-time"}]}"#,
            now,
        )
        .expect("one credit with an unreadable date still counts");
        assert_eq!(only.left, 1);
        assert_eq!(only.expiries, vec![None]);
        // A number is not the string the inventory sends. It is not turned
        // into a datetime.
        let numbered = parse_codex_reset_credits(
            r#"{"available_count":1,"credits":[{"status":"available","expires_at":1780000000}]}"#,
            now,
        )
        .expect("the credit still counts");
        assert_eq!(numbered.left, 1);
        assert_eq!(numbered.expiries, vec![None]);
    }
}
