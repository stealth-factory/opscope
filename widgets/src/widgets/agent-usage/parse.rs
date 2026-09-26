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

/// What a JetBrains IDE last recorded about the AI Assistant quota.
///
/// The IDE writes this itself, into `options/AIAssistantQuotaManager2.xml`
/// under its config directory, each time it asks JetBrains. So it is the
/// account's figure as of the IDE's last look, not this machine's spend.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct JetBrainsQuota {
    /// `quotaInfo.type`, e.g. `Available`.
    pub kind: String,
    pub used: f64,
    pub maximum: f64,
    /// `tariffQuota.available` where the file carries it, otherwise what
    /// `maximum - used` leaves.
    pub available: f64,
    /// `quotaInfo.until`: when the licence the quota belongs to runs out.
    pub until: Option<f64>,
    /// `nextRefill.next`: when the credits come back.
    pub refill: Option<f64>,
    /// How much a refill restores, where stated.
    pub refill_amount: Option<f64>,
    /// The refill period in seconds, where it is an ISO duration.
    pub refill_secs: Option<f64>,
}

impl JetBrainsQuota {
    pub fn used_pct(&self) -> Option<f64> {
        (self.maximum > 0.0).then(|| (self.used / self.maximum * 100.0).clamp(0.0, 100.0))
    }
}

/// `AIAssistantQuotaManager2.xml`, as a JetBrains IDE writes it.
///
/// Two `<option>` elements whose `value` attributes are JSON, entity-encoded
/// into the attribute. Only that one component is read, so this looks for
/// it by name rather than parsing the whole document - which also keeps an
/// XML crate out of the binary. Numbers arrive as strings (`"7478.3"`) and
/// are accepted either way. None when there is no `quotaInfo` to read: a
/// file with no quota in it says nothing, and a zero would say something.
pub fn parse_jetbrains_quota(xml: &str) -> Option<JetBrainsQuota> {
    let component = jetbrains_component(xml)?;
    let info: serde_json::Value =
        serde_json::from_str(&decode_entities(&option_value(component, "quotaInfo")?)).ok()?;
    let used = loose_num(&info["current"]).unwrap_or(0.0);
    let maximum = loose_num(&info["maximum"]).unwrap_or(0.0);
    let available = loose_num(&info["tariffQuota"]["available"])
        .unwrap_or_else(|| (maximum - used).max(0.0));
    let mut out = JetBrainsQuota {
        kind: info["type"].as_str().unwrap_or("").to_string(),
        used,
        maximum,
        available,
        until: info["until"].as_str().and_then(iso_epoch),
        ..Default::default()
    };
    let refill: Option<serde_json::Value> = option_value(component, "nextRefill")
        .and_then(|raw| serde_json::from_str(&decode_entities(&raw)).ok());
    if let Some(r) = refill {
        out.refill = r["next"].as_str().and_then(iso_epoch);
        out.refill_amount = loose_num(&r["amount"]).or_else(|| loose_num(&r["tariff"]["amount"]));
        out.refill_secs = r["duration"]
            .as_str()
            .or_else(|| r["tariff"]["duration"].as_str())
            .and_then(parse_iso_hours);
    }
    Some(out)
}

/// An ISO-8601 duration of the shape JetBrains uses, `PT720H`, in seconds.
///
/// Only day, hour, minute and second parts. A month or year part has no
/// fixed length, so it is refused rather than guessed at, and the pace mark
/// that would have rested on it is not drawn.
pub fn parse_iso_hours(s: &str) -> Option<f64> {
    let rest = s.strip_prefix('P')?;
    let (days, time) = match rest.split_once('T') {
        Some((d, t)) => (d, t),
        None => (rest, ""),
    };
    let mut total = 0.0;
    let mut any = false;
    for (part, units) in [(days, &[('D', 86400.0)][..]), (time, &[('H', 3600.0), ('M', 60.0), ('S', 1.0)][..])] {
        let mut num = String::new();
        for ch in part.chars() {
            if ch.is_ascii_digit() || ch == '.' {
                num.push(ch);
                continue;
            }
            let scale = units.iter().find(|(u, _)| *u == ch)?.1;
            total += num.parse::<f64>().ok()? * scale;
            num.clear();
            any = true;
        }
        if !num.is_empty() {
            return None;
        }
    }
    (any && total > 0.0).then_some(total)
}

fn jetbrains_component(xml: &str) -> Option<&str> {
    let mut from = 0;
    while let Some(at) = xml[from..].find("<component") {
        let start = from + at;
        let open_end = start + xml[start..].find('>')?;
        let tag = &xml[start..open_end];
        if attr(tag, "name").as_deref() == Some("AIAssistantQuotaManager2") {
            let close = xml[open_end..].find("</component>").map_or(xml.len(), |i| open_end + i);
            return Some(&xml[open_end..close]);
        }
        from = open_end;
    }
    None
}

fn option_value(component: &str, name: &str) -> Option<String> {
    let mut from = 0;
    while let Some(at) = component[from..].find("<option") {
        let start = from + at;
        let end = start + component[start..].find('>')?;
        let tag = &component[start..end];
        if attr(tag, "name").as_deref() == Some(name) {
            return attr(tag, "value");
        }
        from = end;
    }
    None
}

/// One attribute out of a start tag, either quote style, any whitespace
/// around the `=` - the IDE puts each attribute on a line of its own.
fn attr(tag: &str, name: &str) -> Option<String> {
    let mut rest = tag;
    while let Some(at) = rest.find(name) {
        let before_ok = rest[..at].ends_with(|c: char| c.is_whitespace());
        let after = rest[at + name.len()..].trim_start();
        if before_ok {
            if let Some(after_eq) = after.strip_prefix('=') {
                let after_eq = after_eq.trim_start();
                let quote = after_eq.chars().next().filter(|c| *c == '"' || *c == '\'')?;
                let body = &after_eq[1..];
                return Some(body[..body.find(quote)?].to_string());
            }
        }
        rest = &rest[at + name.len()..];
    }
    None
}

/// The XML entities an attribute value can carry. `&amp;` goes last, so an
/// encoded `&amp;quot;` comes out as the literal text `&quot;`.
fn decode_entities(s: &str) -> String {
    let mut out = s.to_string();
    for (from, to) in [("&#10;", "\n"), ("&#13;", "\r"), ("&#9;", "\t"), ("&quot;", "\""),
                       ("&apos;", "'"), ("&lt;", "<"), ("&gt;", ">"), ("&amp;", "&")] {
        out = out.replace(from, to);
    }
    out
}

fn loose_num(value: &serde_json::Value) -> Option<f64> {
    value.as_f64().or_else(|| value.as_str()?.trim().parse().ok())
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

    fn jetbrains_file(info: &str, refill: &str) -> String {
        let enc = |s: &str| s.replace('"', "&quot;").replace('\n', "&#10;");
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<application>\n  \
             <component name=\"AIAssistantQuotaManager2\">\n    <option\n      \
             name=\"quotaInfo\"\n      value=\"{}\" />\n    <option\n      \
             name=\"nextRefill\"\n      value=\"{}\" />\n  </component>\n</application>\n",
            enc(info),
            enc(refill)
        )
    }

    #[test]
    fn a_jetbrains_quota_file_gives_used_available_and_the_refill() {
        // The shape an IDE writes: numbers as strings, one attribute a line.
        let xml = jetbrains_file(
            r#"{
  "type": "Available",
  "current": "7478.3",
  "maximum": "1000000",
  "until": "2026-11-09T21:00:00Z",
  "tariffQuota": {"current": "7478.3", "maximum": "1000000", "available": "992521.7"}
}"#,
            r#"{
  "type": "Known",
  "next": "2026-10-16T14:00:54.939Z",
  "tariff": {"amount": "1000000", "duration": "PT720H"}
}"#,
        );
        let q = parse_jetbrains_quota(&xml).expect("parsed");
        assert_eq!(q.kind, "Available");
        assert_eq!(q.used, 7478.3);
        assert_eq!(q.maximum, 1_000_000.0);
        assert_eq!(q.available, 992_521.7);
        assert_eq!(q.until, Some(at("2026-11-09T21:00:00Z")));
        assert_eq!(q.refill, Some(at("2026-10-16T14:00:54.939Z")));
        assert_eq!(q.refill_amount, Some(1_000_000.0));
        assert_eq!(q.refill_secs, Some(30.0 * 86400.0));
        let pct = q.used_pct().unwrap();
        assert!((pct - 0.74783).abs() < 1e-9, "{pct}");
    }

    #[test]
    fn without_a_tariff_quota_what_is_left_is_worked_out() {
        // Older files carry no tariffQuota, and a refill period that is not
        // a duration at all.
        let xml = jetbrains_file(
            r#"{"type": "paid", "current": "50000", "maximum": "100000"}"#,
            r#"{"type": "monthly", "next": "2026-11-01T00:00:00Z",
                "tariff": {"amount": "100000", "duration": "monthly"}}"#,
        );
        let q = parse_jetbrains_quota(&xml).expect("parsed");
        assert_eq!(q.available, 50_000.0);
        assert_eq!(q.used_pct(), Some(50.0));
        // No length claimed for a period that did not state one.
        assert_eq!(q.refill_secs, None);
        assert_eq!(q.refill, Some(at("2026-11-01T00:00:00Z")));
    }

    #[test]
    fn a_file_with_no_quota_component_is_not_a_zero() {
        // Nothing to read is None, so the tab says so instead of drawing 0%.
        assert_eq!(parse_jetbrains_quota("<application/>"), None);
        let other = "<application><component name=\"Other\">\
                     <option name=\"quotaInfo\" value=\"{}\"/></component></application>";
        assert_eq!(parse_jetbrains_quota(other), None);
        // A zero maximum has no percentage to rank.
        let xml = jetbrains_file(r#"{"current": "0", "maximum": "0"}"#, "");
        assert_eq!(parse_jetbrains_quota(&xml).unwrap().used_pct(), None);
    }

    #[test]
    fn iso_durations_with_a_fixed_length_are_read_and_others_refused() {
        assert_eq!(parse_iso_hours("PT720H"), Some(2_592_000.0));
        assert_eq!(parse_iso_hours("P7D"), Some(604_800.0));
        assert_eq!(parse_iso_hours("P1DT12H30M"), Some(131_400.0));
        // Months have no fixed length, and the rest are not durations.
        assert_eq!(parse_iso_hours("P1M"), None);
        assert_eq!(parse_iso_hours("monthly"), None);
        assert_eq!(parse_iso_hours("PT"), None);
        assert_eq!(parse_iso_hours("PT12"), None);
    }
}
