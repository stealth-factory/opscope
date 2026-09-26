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

/// One reset credit still usable on a Codex account.
///
/// `title` is the credit's own `title`. Absent, blank, or not a string is
/// no title, and nothing is put in its place. `expiry` is `expires_at`.
/// `None` means that date could not be read.
#[derive(Clone, Debug, PartialEq)]
pub struct ResetCredit {
    pub title: Option<String>,
    pub expiry: Option<f64>,
}

/// Reset credits still usable on a Codex account.
///
/// `left` is a count the body supports. `credits` is one entry per credit
/// that count was taken from, soonest first. An empty `credits` is a count
/// the server stated without listing the credits, so no date row is drawn.
#[derive(Clone, Debug, PartialEq)]
pub struct ResetBank {
    pub left: u64,
    pub credits: Vec<ResetCredit>,
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
    // Missing or null is not a count, so a readable list can supply one.
    // A number that is negative or not whole rejects the body: that count
    // cannot sit beside the list, and it is not replaced with one.
    let reported = match obj.get("available_count") {
        None | Some(serde_json::Value::Null) => None,
        Some(value) => Some(whole_count(value)?),
    };
    // Null is a count with no list, same as the key being absent. An
    // unreadable list falls through to the count alone below.
    let Some(credits) = obj.get("credits").filter(|value| !value.is_null()) else {
        return Some(ResetBank {
            left: reported?,
            credits: Vec::new(),
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
            credits: Vec::new(),
        });
    }
    let mut listed = Vec::new();
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
        listed.push(ResetCredit {
            title: credit_title(credit),
            expiry,
        });
    }
    listed.sort_by(|a, b| match (a.expiry, b.expiry) {
        (Some(left), Some(right)) => left.total_cmp(&right),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    // The list was readable, so the bank is the credits still in it. A
    // reported count that disagrees is not drawn beside them: the lines
    // under the number have to be that number.
    Some(ResetBank {
        left: listed.len() as u64,
        credits: listed,
    })
}

/// The credit's own title, or nothing. A blank or a non-string is not a title.
///
/// Control characters and terminal sequences are removed before the title
/// is kept. What remains is the readable text. A title that was only a
/// sequence is not a title.
fn credit_title(credit: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    let raw = credit.get("title")?.as_str()?;
    let clean = strip_controls(raw);
    let clean = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    if clean.is_empty() { None } else { Some(clean) }
}

/// Drop terminal sequences and other controls, and keep the readable text.
///
/// A newline or tab is a space, so words on either side stay words. An
/// escape sequence is removed whole, parameters included, so `Full` plus a
/// colour sequence plus `reset` stays `Full reset`.
fn strip_controls(raw: &str) -> String {
    let chars: Vec<char> = raw.chars().collect();
    let mut out = String::with_capacity(raw.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\u{1b}' {
            i = skip_escape(&chars, i);
            continue;
        }
        if c == '\u{9b}' {
            i = skip_csi(&chars, i + 1);
            continue;
        }
        // C1 string introducers: DCS, SOS, OSC, PM, APC. Dropping only the
        // introducer would leave the payload in the title.
        if matches!(c, '\u{90}' | '\u{98}' | '\u{9d}' | '\u{9e}' | '\u{9f}') {
            i = skip_string_sequence(&chars, i + 1);
            continue;
        }
        if matches!(c, '\n' | '\r' | '\t' | '\u{2028}' | '\u{2029}') {
            out.push(' ');
            i += 1;
            continue;
        }
        if c.is_control() {
            i += 1;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// `ESC` and the sequence it introduces. The index returned is the first
/// character that is not part of that sequence.
fn skip_escape(chars: &[char], i: usize) -> usize {
    let Some(next) = chars.get(i + 1).copied() else {
        return chars.len();
    };
    match next {
        '[' => skip_csi(chars, i + 2),
        ']' | 'P' | 'X' | '^' | '_' => skip_string_sequence(chars, i + 2),
        _ => i + 2,
    }
}

fn skip_csi(chars: &[char], mut i: usize) -> usize {
    while i < chars.len() {
        let u = chars[i] as u32;
        if (0x20..=0x3F).contains(&u) {
            i += 1;
            continue;
        }
        if (0x40..=0x7E).contains(&u) {
            return i + 1;
        }
        return i;
    }
    chars.len()
}

/// OSC, DCS, and the other string sequences, through BEL or ST.
///
/// An `ESC` that starts a new sequence ends this one and is left for the
/// caller. An unclosed sequence runs to the end of the title.
fn skip_string_sequence(chars: &[char], mut i: usize) -> usize {
    while i < chars.len() {
        if chars[i] == '\u{7}' || chars[i] == '\u{9c}' {
            return i + 1;
        }
        if chars[i] == '\u{1b}' {
            if chars.get(i + 1) == Some(&'\\') {
                return i + 2;
            }
            return i;
        }
        i += 1;
    }
    chars.len()
}

/// The count alone, once the list can no longer be trusted credit by credit.
fn count_only(reported: Option<u64>) -> Option<ResetBank> {
    Some(ResetBank {
        left: reported?,
        credits: Vec::new(),
    })
}

fn whole_count(value: &serde_json::Value) -> Option<u64> {
    if let Some(n) = value.as_u64() {
        return Some(n);
    }
    value.as_i64().filter(|n| *n >= 0).map(|n| n as u64)
}

/// What `coderabbit usage` reports for the current billing period.
///
/// Every `label : value` line, in the order the CLI printed them, keyed by
/// the label in lower case. The CLI documents the report as the review
/// count, whether usage billing is on, and "when available" the spend and
/// the reset date, so fields this does not name are kept rather than
/// dropped: a spend line the widget has never seen still reaches the tab.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CodeRabbitUsage {
    pub fields: Vec<(String, String)>,
}

impl CodeRabbitUsage {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.fields.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    /// `Your reviews`, when it is a whole number.
    pub fn reviews(&self) -> Option<u64> {
        self.get("your reviews")?.replace(',', "").parse().ok()
    }
}

/// `coderabbit usage`, stdout and stderr together.
///
/// The report is aligned `Label  : value` lines under a title. None when it
/// carries none of the three fields that make it a usage report, which is
/// what a signed-out CLI, a self-hosted login or a changed format all look
/// like - `coderabbit_signed_out` tells the first apart.
pub fn parse_coderabbit_usage(text: &str) -> Option<CodeRabbitUsage> {
    let mut out = CodeRabbitUsage::default();
    for line in text.lines() {
        let line = strip_controls(line);
        let Some((label, value)) = line.split_once(':') else {
            continue;
        };
        let (label, value) = (label.trim().to_lowercase(), value.trim());
        if label.is_empty() || value.is_empty() || out.get(&label).is_some() {
            continue;
        }
        out.fields.push((label, value.to_string()));
    }
    let known = out.reviews().is_some()
        || out.get("usage billing").is_some()
        || out.get("period resets").is_some();
    known.then_some(out)
}

/// Whether the CLI's own words say it is not logged in.
pub fn coderabbit_signed_out(text: &str) -> bool {
    let lower = text.to_lowercase();
    [
        "not authenticated",
        "please log in",
        "auth login",
        "authentication required",
        "unauthorized",
        "no session found",
    ]
    .iter()
    .any(|s| lower.contains(s))
}

/// One Notion AI allowance window, from `getCreditRateLimitStatus`.
///
/// Both numbers come from Notion; neither is assumed. `limit` has been 100
/// in every answer seen, which makes `used` look like a percentage, but a
/// window is kept only as a fraction of the limit it came with, so a
/// different limit keeps working.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NotionWindow {
    pub used: f64,
    pub limit: f64,
    /// The rolling window's length in Notion's own form, `6h`. Empty on the
    /// billing-period window, which states an end instead.
    pub span: String,
    /// When the billing period ends, as epoch seconds.
    pub ends: Option<f64>,
}

impl NotionWindow {
    pub fn pct(&self) -> f64 {
        self.used / self.limit * 100.0
    }
}

/// `getCreditRateLimitStatus`: the Notion AI usage allowance for one member
/// of one workspace - a rolling window and the billing period.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NotionAllowance {
    /// `within_limit`, `not_applicable`, or whatever Notion says next.
    pub status: String,
    pub rolling: Option<NotionWindow>,
    /// Seconds from when this was read until the rolling window resets.
    pub resets_in: Option<f64>,
    pub period: Option<NotionWindow>,
    /// `preview` before Notion began enforcing the allowance.
    pub enforcement: String,
}

impl NotionAllowance {
    /// A plan with no allowance: Free, Plus and personal workspaces.
    pub fn not_applicable(&self) -> bool {
        self.status.eq_ignore_ascii_case("not_applicable")
    }
}

/// `POST /api/v3/getCreditRateLimitStatus`.
///
/// None for a body with no window in it that is not a `not_applicable`.
/// Every field is optional, so an error envelope or a changed shape would
/// otherwise come through as an allowance with nothing used, which reads as
/// plenty of room on a workspace that may be at its cap.
pub fn parse_notion_allowance(text: &str) -> Option<NotionAllowance> {
    let body: serde_json::Value = serde_json::from_str(text).ok()?;
    let window = |v: &serde_json::Value| -> Option<NotionWindow> {
        let (used, limit) = (v["used"].as_f64()?, v["limit"].as_f64()?);
        (limit > 0.0).then(|| NotionWindow {
            used,
            limit,
            span: v["window"].as_str().unwrap_or_default().to_string(),
            ends: v["periodEndMs"].as_f64().filter(|ms| *ms > 0.0).map(|ms| ms / 1000.0),
        })
    };
    let out = NotionAllowance {
        status: body["status"].as_str().unwrap_or_default().to_string(),
        rolling: window(&body["window"]),
        // Zero is a real answer, the window resetting now.
        resets_in: body["resetsInSeconds"].as_f64().filter(|s| *s >= 0.0),
        period: window(&body["billingPeriodWindow"]),
        enforcement: body["enforcement"].as_str().unwrap_or_default().to_string(),
    };
    (out.not_applicable() || out.rolling.is_some() || out.period.is_some()).then_some(out)
}

/// `6h` as seconds. Notion states the rolling window as a number and a unit.
pub fn notion_span_secs(span: &str) -> Option<f64> {
    let span = span.trim().to_lowercase();
    let unit = span.chars().last()?;
    let n: f64 = span[..span.len() - unit.len_utf8()].parse().ok()?;
    if n <= 0.0 {
        return None;
    }
    Some(
        n * match unit {
            'm' => 60.0,
            'h' => 3600.0,
            'd' => 86400.0,
            'w' => 7.0 * 86400.0,
            _ => return None,
        },
    )
}

/// One workspace the signed-in account can see.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NotionSpace {
    pub id: String,
    pub name: String,
    /// `free`, `plus`, `business`, `enterprise`.
    pub tier: String,
}

impl NotionSpace {
    /// Only Business and Enterprise carry a Notion AI allowance.
    pub fn has_allowance(&self) -> bool {
        matches!(self.tier.to_lowercase().as_str(), "business" | "enterprise")
    }
}

/// `POST /api/v3/getSpaces`: the account's email, and its workspaces.
///
/// The answer is a record map keyed by user id. The key used is the one
/// whose own `notion_user` record names it, rather than the first key: a
/// token that sees more than one user would otherwise report another
/// account's allowance under this one's name. An answer naming none is
/// still read when it has only one key, which is how older ones looked.
pub fn parse_notion_spaces(text: &str) -> Option<(String, Vec<NotionSpace>)> {
    let body: serde_json::Value = serde_json::from_str(text).ok()?;
    let root = body.as_object()?;
    // Records arrive as `{"value": {..}}`, and on newer answers nested once more.
    let record = |v: &serde_json::Value| -> serde_json::Value {
        let inner = &v["value"];
        if inner["value"].is_object() {
            inner["value"].clone()
        } else if inner.is_object() {
            inner.clone()
        } else {
            v.clone()
        }
    };
    let named: Vec<&String> = root
        .iter()
        .filter(|(id, v)| record(&v["notion_user"][id.as_str()])["id"].as_str() == Some(id.as_str()))
        .map(|(id, _)| id)
        .collect();
    let user = match named.as_slice() {
        [one] => *one,
        [] if root.len() == 1 => root.keys().next()?,
        _ => return None,
    };
    let held = &root[user];
    let email = record(&held["notion_user"][user.as_str()])["email"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let mut spaces: Vec<NotionSpace> = held["space"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(key, v)| {
            let r = record(v);
            NotionSpace {
                id: r["id"].as_str().unwrap_or(key).to_string(),
                name: r["name"].as_str().unwrap_or_default().to_string(),
                tier: r["subscription_tier"].as_str().unwrap_or_default().to_string(),
            }
        })
        .collect();
    // Map order is not an order anyone chose; the id is at least stable.
    spaces.sort_by(|a, b| a.id.cmp(&b.id));
    Some((email, spaces))
}

/// The workspace to ask about: the one named, when the account can see it;
/// otherwise the first that has an allowance; otherwise the first.
///
/// A named id the account cannot see is almost always a typo, and asking
/// about it gets only an opaque refusal - the caller says it was not found.
/// Ids match with or without their dashes, in either case.
pub fn pick_notion_space<'a>(spaces: &'a [NotionSpace], wanted: &str) -> Option<&'a NotionSpace> {
    let bare = |s: &str| s.replace('-', "").to_lowercase();
    let wanted = bare(wanted.trim());
    if !wanted.is_empty() {
        if let Some(hit) = spaces.iter().find(|s| bare(&s.id) == wanted) {
            return Some(hit);
        }
    }
    spaces.iter().find(|s| s.has_allowance()).or_else(|| spaces.first())
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
                 "title":"  ",
                 "expires_at":"2026-07-18T00:39:53.731630Z"},
                {"status":"available","reset_type":"codex_rate_limits",
                 "title":"Full reset",
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
        assert_eq!(bank.credits, vec![
            ResetCredit {
                title: Some("Full reset".into()),
                expiry: Some(at("2026-07-12T04:03:43.263391Z")),
            },
            ResetCredit {
                title: None,
                expiry: Some(at("2026-07-18T00:39:53.731630Z")),
            },
            ResetCredit {
                title: None,
                expiry: None,
            },
        ]);
    }

    #[test]
    fn a_title_keeps_its_words_and_drops_control_sequences() {
        let now = at("2026-07-01T00:00:00Z");
        let coloured = parse_codex_reset_credits(
            "{\"available_count\":1,\"credits\":[{\"status\":\"available\",\
             \"title\":\"Full\\u001b[31m reset\",\"expires_at\":\"2026-07-12T00:00:00Z\"}]}",
            now,
        )
        .expect("the coloured title");
        assert_eq!(coloured.credits[0].title.as_deref(), Some("Full reset"));

        let only = parse_codex_reset_credits(
            "{\"available_count\":1,\"credits\":[{\"status\":\"available\",\
             \"title\":\"\\u001b[31m\",\"expires_at\":\"2026-07-12T00:00:00Z\"}]}",
            now,
        )
        .expect("a sequence is not a title");
        assert_eq!(only.credits[0].title, None);

        let osc = parse_codex_reset_credits(
            "{\"available_count\":1,\"credits\":[{\"status\":\"available\",\
             \"title\":\"Full\\u001b]0;x\\u0007 reset\",\"expires_at\":\"2026-07-12T00:00:00Z\"}]}",
            now,
        )
        .expect("the osc title");
        assert_eq!(osc.credits[0].title.as_deref(), Some("Full reset"));

        let broken = parse_codex_reset_credits(
            "{\"available_count\":1,\"credits\":[{\"status\":\"available\",\
             \"title\":\"Full\\nreset\",\"expires_at\":\"2026-07-12T00:00:00Z\"}]}",
            now,
        )
        .expect("the broken title");
        assert_eq!(broken.credits[0].title.as_deref(), Some("Full reset"));
        assert!(
            !broken.credits[0]
                .title
                .as_deref()
                .unwrap()
                .chars()
                .any(char::is_control),
            "a control reached the title"
        );

        let c1 = parse_codex_reset_credits(
            "{\"available_count\":1,\"credits\":[{\"status\":\"available\",\
             \"title\":\"Full\\u009b31m reset\",\"expires_at\":\"2026-07-12T00:00:00Z\"}]}",
            now,
        )
        .expect("the c1 title");
        assert_eq!(c1.credits[0].title.as_deref(), Some("Full reset"));

        let c1_osc = parse_codex_reset_credits(
            "{\"available_count\":1,\"credits\":[{\"status\":\"available\",\
             \"title\":\"Full\\u009d0;x\\u0007 reset\",\"expires_at\":\"2026-07-12T00:00:00Z\"}]}",
            now,
        )
        .expect("the c1 osc title");
        assert_eq!(c1_osc.credits[0].title.as_deref(), Some("Full reset"));
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
        assert!(bank.credits.is_empty());
    }

    #[test]
    fn a_null_credit_list_keeps_the_count_and_invents_no_expiry() {
        let bank = parse_codex_reset_credits(r#"{"available_count":2,"credits":null}"#, 0.0)
            .expect("the count");
        assert_eq!(bank.left, 2);
        assert!(bank.credits.is_empty(), "null is not a list of credits");
        assert!(parse_codex_reset_credits(r#"{"credits":null}"#, 0.0).is_none());
    }

    #[test]
    fn a_null_count_lets_the_readable_list_supply_the_number() {
        let now = at("2026-07-01T00:00:00Z");
        let bank = parse_codex_reset_credits(
            r#"{"available_count":null,"credits":[
                {"status":"available","expires_at":"2026-07-12T00:00:00Z"},
                {"status":"redeemed","expires_at":"2026-08-01T00:00:00Z"}
            ]}"#,
            now,
        )
        .expect("the list is the count");
        assert_eq!(bank.left, 1);
        assert_eq!(bank.credits, vec![ResetCredit {
            title: None,
            expiry: Some(at("2026-07-12T00:00:00Z")),
        }]);
        assert!(
            parse_codex_reset_credits(
                r#"{"available_count":2.5,"credits":[
                    {"status":"available","expires_at":"2026-07-12T00:00:00Z"}
                ]}"#,
                now,
            )
            .is_none(),
            "a fractional count is not replaced by the list"
        );
    }

    #[test]
    fn an_empty_list_keeps_the_count_the_server_stated() {
        let now = at("2026-07-01T00:00:00Z");
        let none = parse_codex_reset_credits(r#"{"credits":[],"available_count":0}"#, now)
            .expect("a real zero");
        assert_eq!(none.left, 0);
        assert!(none.credits.is_empty());
        let stated = parse_codex_reset_credits(r#"{"credits":[],"available_count":2}"#, now)
            .expect("a count without the credits listed");
        assert_eq!(stated.left, 2);
        assert!(
            stated.credits.is_empty(),
            "no expiry was sent, so none is drawn"
        );
    }

    #[test]
    fn a_summary_with_only_a_count_is_a_bank_without_expiries() {
        let bank = parse_codex_reset_credits(r#"{"available_count":4}"#, 0.0).expect("a summary");
        assert_eq!(bank.left, 4);
        assert!(bank.credits.is_empty());
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
        assert_eq!(bank.credits, vec![
            ResetCredit {
                title: None,
                expiry: Some(at("2026-07-12T00:00:00Z")),
            },
            ResetCredit {
                title: None,
                expiry: None,
            },
        ]);
        let only = parse_codex_reset_credits(
            r#"{"credits":[{"status":"available","expires_at":"not-a-time"}]}"#,
            now,
        )
        .expect("one credit with an unreadable date still counts");
        assert_eq!(only.left, 1);
        assert_eq!(only.credits, vec![ResetCredit {
            title: None,
            expiry: None,
        }]);
        // A number is not the string the inventory sends. It is not turned
        // into a datetime.
        let numbered = parse_codex_reset_credits(
            r#"{"available_count":1,"credits":[{"status":"available","expires_at":1780000000}]}"#,
            now,
        )
        .expect("the credit still counts");
        assert_eq!(numbered.left, 1);
        assert_eq!(numbered.credits, vec![ResetCredit {
            title: None,
            expiry: None,
        }]);
    }

    #[test]
    fn a_coderabbit_report_gives_its_fields_in_order() {
        // The shape the CLI prints, colour and all.
        let text = "\u{1b}[1mCodeRabbit Usage — current billing period\u{1b}[0m\n\n\
                    Organization  : Example Org\n\
                    Usage billing : inactive\n\
                    User          : example-user\n\
                    Your reviews  : 1,025\n\
                    Spend         : $4.20\n\
                    Period resets : 2026-09-30\n";
        let u = parse_coderabbit_usage(text).expect("parsed");
        assert_eq!(u.reviews(), Some(1025));
        assert_eq!(u.get("organization"), Some("Example Org"));
        assert_eq!(u.get("usage billing"), Some("inactive"));
        assert_eq!(u.get("period resets"), Some("2026-09-30"));
        // A field this does not name is kept, not dropped.
        assert_eq!(u.get("spend"), Some("$4.20"));
        assert_eq!(u.fields[0].0, "organization");
    }

    #[test]
    fn a_coderabbit_answer_with_no_usage_fields_is_not_a_report() {
        // Signed out: an error line with a colon in it is still not a report.
        let text = "Error: not authenticated. Run `coderabbit auth login`.";
        assert_eq!(parse_coderabbit_usage(text), None);
        assert!(coderabbit_signed_out(text));
        assert!(!coderabbit_signed_out("Your reviews : 3"));
        // A count that is not a number does not make a report on its own.
        assert_eq!(parse_coderabbit_usage("Your reviews : lots"), None);
    }

    const NOTION_STATUS: &str = r#"{
      "status": "within_limit",
      "window": { "creditType": "basic_ai_credits", "scope": "per_user", "window": "6h", "used": 42.5, "limit": 100 },
      "resetsInSeconds": 12600,
      "billingPeriodWindow": { "creditType": "basic_ai_credits", "scope": "per_user",
        "cadence": "billing_period", "used": 18.0, "limit": 100, "periodEndMs": 1788000000000 },
      "enforcement": "preview"
    }"#;

    #[test]
    fn a_notion_allowance_gives_both_windows_against_their_own_limits() {
        let a = parse_notion_allowance(NOTION_STATUS).unwrap();
        let rolling = a.rolling.as_ref().unwrap();
        assert_eq!((rolling.used, rolling.limit, rolling.span.as_str()), (42.5, 100.0, "6h"));
        assert_eq!(a.resets_in, Some(12600.0));
        let period = a.period.as_ref().unwrap();
        assert_eq!(period.ends, Some(1_788_000_000.0));
        assert_eq!(a.enforcement, "preview");
        // A limit that is not 100 is still a fraction of that limit.
        let other = NOTION_STATUS.replace("\"used\": 42.5, \"limit\": 100", "\"used\": 50, \"limit\": 200");
        assert_eq!(parse_notion_allowance(&other).unwrap().rolling.unwrap().pct(), 25.0);
    }

    #[test]
    fn a_notion_answer_with_no_window_is_not_an_allowance() {
        // An error envelope must not read as nothing used.
        assert!(parse_notion_allowance(r#"{"errorId":"x","name":"UnauthorizedError"}"#).is_none());
        assert!(parse_notion_allowance("<html>").is_none());
        // A zero limit has no fraction to draw.
        assert!(parse_notion_allowance(r#"{"window":{"used":1,"limit":0}}"#).is_none());
        // A plan with no allowance says so, and that is an answer.
        let none = parse_notion_allowance(r#"{"status":"not_applicable"}"#).unwrap();
        assert!(none.not_applicable() && none.rolling.is_none());
    }

    #[test]
    fn a_notion_window_length_is_read_from_its_own_token() {
        assert_eq!(notion_span_secs("6h"), Some(21600.0));
        assert_eq!(notion_span_secs("30m"), Some(1800.0));
        assert_eq!(notion_span_secs("1d"), Some(86400.0));
        for bad in ["", "h", "6", "0h", "6y"] {
            assert_eq!(notion_span_secs(bad), None, "{bad}");
        }
    }

    const NOTION_SPACES: &str = r#"{
      "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee": {
        "notion_user": { "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee":
          { "value": { "value": { "id": "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee", "email": "person@example.com" } } } },
        "space": {
          "66666666-7777-8888-9999-aaaaaaaaaaaa": { "value": { "value":
            { "id": "66666666-7777-8888-9999-aaaaaaaaaaaa", "name": "Personal", "subscription_tier": "free" } } },
          "11111111-2222-3333-4444-555555555555": { "value":
            { "id": "11111111-2222-3333-4444-555555555555", "name": "Acme", "subscription_tier": "business" } }
        }
      }
    }"#;

    #[test]
    fn notion_spaces_are_read_for_the_user_the_answer_names() {
        let (email, spaces) = parse_notion_spaces(NOTION_SPACES).unwrap();
        assert_eq!(email, "person@example.com");
        let names: Vec<&str> = spaces.iter().map(|s| s.name.as_str()).collect();
        // Both record shapes, the nested and the flat one.
        assert_eq!(names, ["Acme", "Personal"]);
        // Two users and neither naming itself is ambiguous, so it is refused
        // rather than guessed at.
        let two = r#"{"u1":{"space":{}},"u2":{"space":{}}}"#;
        assert!(parse_notion_spaces(two).is_none());
        // One key naming nobody is how older answers looked.
        assert_eq!(parse_notion_spaces(r#"{"u1":{"space":{}}}"#).unwrap().1.len(), 0);
    }

    #[test]
    fn the_notion_workspace_asked_about_is_the_named_one_else_one_with_an_allowance() {
        let (_, spaces) = parse_notion_spaces(NOTION_SPACES).unwrap();
        assert_eq!(pick_notion_space(&spaces, "").unwrap().name, "Acme");
        // Named, with or without dashes, in any case.
        assert_eq!(
            pick_notion_space(&spaces, "66666666777788889999AAAAAAAAAAAA").unwrap().name,
            "Personal"
        );
        assert_eq!(
            pick_notion_space(&spaces, "66666666-7777-8888-9999-aaaaaaaaaaaa").unwrap().name,
            "Personal"
        );
        // A name the account cannot see falls back to the automatic choice.
        assert_eq!(pick_notion_space(&spaces, "nope").unwrap().name, "Acme");
        assert!(pick_notion_space(&[], "").is_none());
    }
}
