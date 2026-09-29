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
pub(crate) fn strip_controls(raw: &str) -> String {
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

    /// The first field whose label says it is `kind` of the rolling quota.
    fn quota_field(&self, kind: QuotaField) -> Option<&str> {
        self.fields
            .iter()
            .find(|(k, _)| coderabbit_quota_field(k) == Some(kind))
            .map(|(_, v)| v.as_str())
    }

    /// Included reviews left in the rolling window, and out of how many
    /// when the report says. CLI 0.8 added this; 0.7 printed a count only.
    pub fn available(&self) -> Option<(u64, Option<u64>)> {
        let nums = leading_numbers(self.quota_field(QuotaField::Available)?);
        let left = *nums.first()?;
        let of = nums.get(1).copied().or_else(|| {
            leading_numbers(self.quota_field(QuotaField::Limit)?).first().copied()
        });
        // A limit below what is left is not a limit this count is a share
        // of, and would draw an empty bar beside `7 of 5`.
        Some((left, of.filter(|n| *n > 0 && left <= *n)))
    }

    /// CodeRabbit's own reason, when it says the included reviews could not
    /// be checked - `Availability : unavailable` beside a `Note`. The
    /// billing period still arrives with it, so the report is not a failure.
    pub fn unavailable_why(&self) -> Option<String> {
        let said = self.quota_field(QuotaField::Available)?;
        if !said.to_lowercase().starts_with("unavailable") {
            return None;
        }
        Some(self.get("note").unwrap_or(said).trim_end_matches('.').to_string())
    }

    /// How long the rolling window is, in seconds.
    pub fn window_secs(&self) -> Option<f64> {
        coderabbit_span_secs(self.quota_field(QuotaField::Window)?)
    }

    /// When capacity comes back, as epoch seconds: a stamp as given, or a
    /// span counted from `read_at`, the moment the report was taken.
    pub fn returns_at(&self, read_at: f64) -> Option<f64> {
        let v = self.quota_field(QuotaField::Returns)?;
        let v = v.trim().trim_start_matches("in ").trim();
        iso_epoch(v).or_else(|| coderabbit_span_secs(v).map(|s| read_at + s))
    }
}

/// Which part of the rolling quota a report line is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum QuotaField {
    Available,
    Limit,
    Window,
    Returns,
}

/// What a `coderabbit usage` label says it is, if it is part of the
/// rolling quota. Read from words, not exact labels: CLI 0.8's wording is
/// documented only as "available included reviews, the rolling quota
/// window, and when capacity returns", so the label's words are matched
/// rather than a spelling this widget has not seen. The order matters -
/// "capacity returns" and "available again" are a time, not a count.
pub fn coderabbit_quota_field(label: &str) -> Option<QuotaField> {
    let l = label.to_lowercase();
    if l.contains("return") || l.contains("again") || l.contains("refill") || l.contains("next") {
        Some(QuotaField::Returns)
    } else if l.contains("window") {
        Some(QuotaField::Window)
    } else if l.contains("availab") || l.contains("remaining") || l.contains("left") {
        Some(QuotaField::Available)
    } else if l.contains("limit") || l.contains("quota") || l.contains("allowance") {
        Some(QuotaField::Limit)
    } else {
        None
    }
}

/// The whole numbers at the front of a value, as in `3 of 5`, `3/5` or
/// `5 per hour`. Stops at the first word that is not a number or a joiner,
/// so a count is never taken from a sentence that follows it.
fn leading_numbers(value: &str) -> Vec<u64> {
    let mut out = Vec::new();
    for word in value.replace('/', " / ").replace(',', "").split_whitespace() {
        match word.trim_matches(|c: char| c == '(' || c == ')') {
            "of" | "/" | "out" => continue,
            w => match w.parse() {
                Ok(n) => out.push(n),
                Err(_) => break,
            },
        }
    }
    out
}

/// A span in words or short units - `1 hour`, `60 minutes`, `1h 5m`,
/// `rolling 1 hour` - in seconds. None when there is no number with a unit.
pub fn coderabbit_span_secs(value: &str) -> Option<f64> {
    let v = value.to_lowercase();
    let mut total = 0.0;
    let mut found = false;
    let mut num: Option<f64> = None;
    // Split `1h5m` into `1 h 5 m` so numbers and units are words of their own.
    let mut spaced = String::new();
    let mut prev_digit = None;
    for c in v.chars() {
        let digit = c.is_ascii_digit() || c == '.';
        if prev_digit.is_some_and(|p| p != digit) && !c.is_whitespace() {
            spaced.push(' ');
        }
        spaced.push(c);
        prev_digit = if c.is_whitespace() { None } else { Some(digit) };
    }
    for word in spaced.split_whitespace() {
        if let Ok(n) = word.parse::<f64>() {
            num = Some(n);
            continue;
        }
        let unit = match word.trim_end_matches([',', '.']) {
            "s" | "sec" | "secs" | "second" | "seconds" => 1.0,
            "m" | "min" | "mins" | "minute" | "minutes" => 60.0,
            "h" | "hr" | "hrs" | "hour" | "hours" => 3600.0,
            "d" | "day" | "days" => 86400.0,
            _ => {
                num = None;
                continue;
            }
        };
        if let Some(n) = num.take() {
            total += n * unit;
            found = true;
        } else if word.starts_with("hour") && !found {
            // `rolling hour`: one of the unit, with no number said.
            total += unit;
            found = true;
        }
    }
    (found && total > 0.0).then_some(total)
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
        || out.get("period resets").is_some()
        || out.available().is_some();
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
            // Drawn as the lane's label, so it may carry nothing the
            // terminal would act on.
            span: strip_controls(v["window"].as_str().unwrap_or_default()),
            ends: v["periodEndMs"].as_f64().filter(|ms| *ms > 0.0).map(|ms| ms / 1000.0),
        })
    };
    let out = NotionAllowance {
        status: strip_controls(body["status"].as_str().unwrap_or_default()),
        rolling: window(&body["window"]),
        // Zero is a real answer, the window resetting now.
        resets_in: body["resetsInSeconds"].as_f64().filter(|s| *s >= 0.0),
        period: window(&body["billingPeriodWindow"]),
        enforcement: strip_controls(body["enforcement"].as_str().unwrap_or_default()),
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
    // Everything drawn from this answer reaches the terminal, so none of it
    // may carry a sequence the terminal would act on.
    let email = strip_controls(
        record(&held["notion_user"][user.as_str()])["email"]
            .as_str()
            .unwrap_or_default(),
    );
    let mut spaces: Vec<NotionSpace> = held["space"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(key, v)| {
            let r = record(v);
            NotionSpace {
                id: strip_controls(r["id"].as_str().unwrap_or(key)),
                name: strip_controls(r["name"].as_str().unwrap_or_default()),
                tier: strip_controls(r["subscription_tier"].as_str().unwrap_or_default()),
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

/// One Devin quota window. `pct` is the number the body sent, on a 0–100
/// scale. `reset` is an absolute instant, when one was sent.
#[derive(Clone, Debug, PartialEq)]
pub struct DevinWindow {
    pub pct: f64,
    pub reset: Option<f64>,
}

/// Devin `GET /api/<org>/billing/quota/usage`.
///
/// `daily` is absent both when the body had no daily window and when
/// `hide_daily_quota` is boolean true. Those two are told apart by
/// `daily_hidden`: a hidden window is not a 0%.
#[derive(Clone, Debug, PartialEq)]
pub struct DevinQuota {
    pub daily: Option<DevinWindow>,
    pub weekly: Option<DevinWindow>,
    pub daily_hidden: bool,
    /// Present only when `overage_balance` or `overage_balance_cents` was in
    /// the body. A missing key is not a balance of zero.
    pub balance: Option<f64>,
    pub plan: Option<String>,
}

/// The flat quota object, or `quota_usage.daily_quota` / `weekly_quota`
/// when the matching top-level percentage is absent.
///
/// None when both windows are missing, including when daily is hidden and
/// weekly was not sent. Percentages are kept as sent: a value below 1 is
/// not multiplied by 100, because 0.4% and 40% are different readings.
pub fn parse_devin_quota(text: &str) -> Option<DevinQuota> {
    let body: serde_json::Value = serde_json::from_str(text).ok()?;
    let obj = body.as_object()?;
    let daily_hidden = obj.get("hide_daily_quota").and_then(|v| v.as_bool()) == Some(true);
    let nested = obj.get("quota_usage");
    let mut daily = devin_window(
        obj,
        nested,
        "daily_percentage",
        "daily_reset_at",
        "daily_quota",
    );
    let weekly = devin_window(
        obj,
        nested,
        "weekly_percentage",
        "weekly_reset_at",
        "weekly_quota",
    );
    if daily_hidden {
        daily = None;
    }
    if daily.is_none() && weekly.is_none() {
        return None;
    }
    Some(DevinQuota {
        daily,
        weekly,
        daily_hidden,
        balance: devin_balance(obj),
        plan: devin_plan(obj),
    })
}

fn devin_window(
    body: &serde_json::Map<String, serde_json::Value>,
    nested: Option<&serde_json::Value>,
    pct_key: &str,
    reset_key: &str,
    nested_key: &str,
) -> Option<DevinWindow> {
    let top = match body.get(pct_key) {
        None | Some(serde_json::Value::Null) => None,
        Some(v) => v.as_f64().filter(|n| n.is_finite() && *n >= 0.0),
    };
    let nested_window = nested
        .and_then(|v| v.get(nested_key))
        .and_then(devin_nested_window);
    let pct = top.or_else(|| nested_window.as_ref().map(|w| w.pct))?;
    let reset = body
        .get(reset_key)
        .and_then(|v| epoch_from(v, 10_000_000_000.0))
        .or_else(|| nested_window.and_then(|w| w.reset));
    Some(DevinWindow { pct, reset })
}

/// Percent, then an inverted remaining percent, then used-over-limit, then
/// remaining-against-limit. Only the keys named here, and only on this
/// object: a walk that matches "day" inside some other key binds the wrong
/// window.
fn devin_nested_window(v: &serde_json::Value) -> Option<DevinWindow> {
    if let Some(n) = v.as_f64().filter(|n| n.is_finite() && *n >= 0.0) {
        return Some(DevinWindow {
            pct: n,
            reset: None,
        });
    }
    let obj = v.as_object()?;
    let pct = named_percent(obj, DEVIN_PERCENT_KEYS)
        .or_else(|| {
            named_percent(obj, DEVIN_REMAINING_PERCENT_KEYS)
                .filter(|n| (0.0..=100.0).contains(n))
                .map(|n| 100.0 - n)
        })
        .or_else(|| ratio_percent(obj, DEVIN_USED_KEYS, DEVIN_LIMIT_KEYS))
        .or_else(|| remaining_percent(obj))?;
    let reset = obj.iter().find_map(|(key, value)| {
        key.to_ascii_lowercase()
            .contains("reset")
            .then(|| epoch_from(value, 10_000_000_000.0))
            .flatten()
    });
    Some(DevinWindow { pct, reset })
}

const DEVIN_PERCENT_KEYS: &[&str] = &[
    "used_percent",
    "usedPercent",
    "usage_percent",
    "usagePercent",
    "percent_used",
    "percentUsed",
    "percent",
];
const DEVIN_REMAINING_PERCENT_KEYS: &[&str] = &[
    "remaining_percent",
    "remainingPercent",
    "percent_remaining",
    "percentRemaining",
];
const DEVIN_USED_KEYS: &[&str] = &["used", "usage", "used_count", "usedCount", "consumed"];
const DEVIN_LIMIT_KEYS: &[&str] = &["limit", "quota", "total", "max", "available"];
const DEVIN_LEFT_KEYS: &[&str] = &["remaining", "left", "available"];
const DEVIN_PLAN_KEYS: &[&str] = &[
    "plan_name",
    "planName",
    "plan",
    "tier",
    "subscription_tier",
    "subscriptionTier",
];

fn named_percent(obj: &serde_json::Map<String, serde_json::Value>, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|key| {
        obj.get(*key)
            .and_then(|v| v.as_f64())
            .filter(|n| n.is_finite() && *n >= 0.0)
    })
}

/// Two counts on this object. The keys have to differ: `available` is on
/// both lists, and one number read as both sides is a 0% nobody sent.
fn ratio_percent(
    obj: &serde_json::Map<String, serde_json::Value>,
    used_keys: &[&str],
    limit_keys: &[&str],
) -> Option<f64> {
    let (used_key, used) = first_keyed(obj, used_keys)?;
    let (limit_key, limit) = first_keyed(obj, limit_keys)?;
    (used_key != limit_key && limit > 0.0 && used >= 0.0).then_some(used / limit * 100.0)
}

fn remaining_percent(obj: &serde_json::Map<String, serde_json::Value>) -> Option<f64> {
    let (left_key, left) = first_keyed(obj, DEVIN_LEFT_KEYS)?;
    let (limit_key, limit) = first_keyed(obj, DEVIN_LIMIT_KEYS)?;
    (left_key != limit_key && limit > 0.0 && left >= 0.0 && left <= limit)
        .then_some((limit - left) / limit * 100.0)
}

fn first_keyed<'a>(
    obj: &serde_json::Map<String, serde_json::Value>,
    keys: &'a [&'a str],
) -> Option<(&'a str, f64)> {
    keys.iter().find_map(|key| {
        let n = obj.get(*key)?.as_f64().filter(|n| n.is_finite())?;
        Some((*key, n))
    })
}

/// `overage_balance` when that key was sent, otherwise cents divided by 100.
/// Either key missing is not filled in with zero.
fn devin_balance(obj: &serde_json::Map<String, serde_json::Value>) -> Option<f64> {
    if let Some(value) = obj.get("overage_balance") {
        return value.as_f64().filter(|n| n.is_finite() && *n >= 0.0);
    }
    obj.get("overage_balance_cents")
        .and_then(|v| v.as_f64())
        .filter(|n| n.is_finite() && *n >= 0.0)
        .map(|cents| cents / 100.0)
}

/// A plan string from the top level only. A nested copy, and the unread
/// flags beside the quota, are not a plan.
fn devin_plan(obj: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    DEVIN_PLAN_KEYS
        .iter()
        .find_map(|key| non_empty(obj.get(*key)?))
}

/// One Factory rate-limit window. `seconds_remaining` is set only when the
/// body sent a positive count. `window_end` is the instant even when it is
/// already past: a closed window and the percent that was sent are different
/// facts, and the percent is not replaced with 0 here.
#[derive(Clone, Debug, PartialEq)]
pub struct FactoryWindow {
    pub pct: f64,
    pub seconds_remaining: Option<f64>,
    pub window_end: Option<f64>,
}

/// Standard is the three named windows. Core is the same shape, and each of
/// its windows can be absent without the others becoming 0%.
#[derive(Clone, Debug, PartialEq)]
pub struct FactoryPool {
    pub five_hour: Option<FactoryWindow>,
    pub weekly: Option<FactoryWindow>,
    pub monthly: Option<FactoryWindow>,
}

/// Factory `GET /api/billing/limits` when the account is on token rate limits.
#[derive(Clone, Debug, PartialEq)]
pub struct FactoryBilling {
    pub standard: FactoryPool,
    pub core: Option<FactoryPool>,
    /// `extraUsageBalanceCents / 100`, and only when that key was sent.
    pub balance: Option<f64>,
    pub overage_preference: Option<String>,
}

/// None unless `usesTokenRateLimitsBilling` is boolean true and at least
/// one standard window has `usedPercent`. A window the server left out is
/// not a 0% bar. When no standard window can be drawn, this is not the
/// rate-limit body, and the older usage endpoint can still be read.
pub fn parse_factory_billing_limits(text: &str) -> Option<FactoryBilling> {
    let body: serde_json::Value = serde_json::from_str(text).ok()?;
    if body
        .get("usesTokenRateLimitsBilling")
        .and_then(|v| v.as_bool())
        != Some(true)
    {
        return None;
    }
    let standard_obj = body.get("limits")?.get("standard")?.as_object()?;
    let standard = FactoryPool {
        five_hour: standard_obj.get("fiveHour").and_then(factory_window),
        weekly: standard_obj.get("weekly").and_then(factory_window),
        monthly: standard_obj.get("monthly").and_then(factory_window),
    };
    if standard.five_hour.is_none() && standard.weekly.is_none() && standard.monthly.is_none() {
        return None;
    }
    let core = body
        .get("limits")
        .and_then(|v| v.get("core"))
        .and_then(factory_core);
    let balance = match body.get("extraUsageBalanceCents") {
        Some(v) => v
            .as_f64()
            .filter(|n| n.is_finite())
            .map(|cents| cents / 100.0),
        None => None,
    };
    Some(FactoryBilling {
        standard,
        core,
        balance,
        overage_preference: body.get("overagePreference").and_then(non_empty),
    })
}

fn factory_window(v: &serde_json::Value) -> Option<FactoryWindow> {
    let obj = v.as_object()?;
    let pct = obj
        .get("usedPercent")?
        .as_f64()
        .filter(|n| n.is_finite() && *n >= 0.0)?;
    let seconds_remaining = obj
        .get("secondsRemaining")
        .and_then(|v| v.as_f64())
        .filter(|n| n.is_finite() && *n > 0.0);
    Some(FactoryWindow {
        pct,
        seconds_remaining,
        window_end: obj.get("windowEnd").and_then(|v| epoch_from(v, 1e12)),
    })
}

/// Core is drawn only when some window has a percent above zero, a
/// `windowEnd`, or a `secondsRemaining` above zero. A window that fails
/// that test is left out even when a neighbour qualifies. A 0% that passes
/// it is kept. Three explicit zeros and no dates are an empty pool, not
/// three bars.
fn factory_core(v: &serde_json::Value) -> Option<FactoryPool> {
    let obj = v.as_object()?;
    let keep = |key: &str| {
        obj.get(key)
            .filter(|window| core_window_has_data(window))
            .and_then(factory_window)
    };
    let pool = FactoryPool {
        five_hour: keep("fiveHour"),
        weekly: keep("weekly"),
        monthly: keep("monthly"),
    };
    (pool.five_hour.is_some() || pool.weekly.is_some() || pool.monthly.is_some()).then_some(pool)
}

fn core_window_has_data(v: &serde_json::Value) -> bool {
    let Some(obj) = v.as_object() else {
        return false;
    };
    if obj
        .get("usedPercent")
        .and_then(|v| v.as_f64())
        .is_some_and(|n| n.is_finite() && n > 0.0)
    {
        return true;
    }
    if obj
        .get("windowEnd")
        .is_some_and(|v| !v.is_null() && epoch_from(v, 1e12).is_some())
    {
        return true;
    }
    // Zero and negative are not a countdown. `factory_window` drops them,
    // and a 0% kept only by that field would be a bar with no reset.
    obj.get("secondsRemaining")
        .and_then(|v| v.as_f64())
        .is_some_and(|n| n.is_finite() && n > 0.0)
}

/// One legacy Standard or Premium pool. `pct` is absent when the body had
/// no `usedRatio` on a 0–1 scale and no positive allowance to divide by.
/// `unlimited` is an allowance above one trillion: there is no denominator,
/// and no bar is filled in for it.
#[derive(Clone, Debug, PartialEq)]
pub struct FactoryTokens {
    pub pct: Option<f64>,
    pub unlimited: bool,
    /// `orgTotalTokensUsed`, only when that key was sent. It is a different
    /// population from the user percent and is not added into it.
    pub org_tokens: Option<f64>,
}

/// Factory `GET /api/organization/subscription/usage`.
#[derive(Clone, Debug, PartialEq)]
pub struct FactoryUsage {
    /// `usage.endDate`, as epoch seconds. Shared by standard and premium.
    pub end: Option<f64>,
    pub standard: FactoryTokens,
    pub premium: FactoryTokens,
}

/// None when the body has no `usage` object. A pool with no ratio and no
/// positive allowance keeps `pct: None` rather than becoming 0%.
pub fn parse_factory_usage(text: &str) -> Option<FactoryUsage> {
    let body: serde_json::Value = serde_json::from_str(text).ok()?;
    let usage = body.get("usage")?.as_object()?;
    Some(FactoryUsage {
        end: usage.get("endDate").and_then(|v| epoch_from(v, 1e12)),
        standard: factory_tokens(usage.get("standard")),
        premium: factory_tokens(usage.get("premium")),
    })
}

fn factory_tokens(v: Option<&serde_json::Value>) -> FactoryTokens {
    let Some(obj) = v.and_then(|v| v.as_object()) else {
        return FactoryTokens {
            pct: None,
            unlimited: false,
            org_tokens: None,
        };
    };
    let allowance = obj
        .get("totalAllowance")
        .and_then(|v| v.as_f64())
        .filter(|n| n.is_finite());
    let tokens = obj
        .get("userTokens")
        .and_then(|v| v.as_f64())
        .filter(|n| n.is_finite());
    let ratio = obj
        .get("usedRatio")
        .and_then(|v| v.as_f64())
        .filter(|n| n.is_finite());
    let org_tokens = match obj.get("orgTotalTokensUsed") {
        Some(v) => v.as_f64().filter(|n| n.is_finite() && *n >= 0.0),
        None => None,
    };
    // Above one trillion the allowance is not a ceiling anyone can draw a
    // bar against. A stand-in denominator would be a percent the body did
    // not have.
    if allowance.is_some_and(|n| n > 1e12) {
        return FactoryTokens {
            pct: None,
            unlimited: true,
            org_tokens,
        };
    }
    let pct = match ratio {
        Some(r) if (0.0..=1.0).contains(&r) => {
            let zero_but_spent =
                r == 0.0 && tokens.is_some_and(|n| n > 0.0) && allowance.is_some_and(|n| n >= 1.0);
            if zero_but_spent {
                Some(tokens.unwrap_or(0.0) / allowance.unwrap_or(1.0) * 100.0)
            } else {
                Some(r * 100.0)
            }
        }
        // A ratio outside 0–1 is not treated as an already-percent. That
        // second scale is unconfirmed, and a guess would draw a different
        // number from the one that was sent.
        _ => match (tokens, allowance) {
            (Some(t), Some(a)) if t >= 0.0 && a > 0.0 => Some(t / a * 100.0),
            _ => None,
        },
    };
    FactoryTokens {
        pct,
        unlimited: false,
        org_tokens,
    }
}

/// What `GET /api/app/auth/me` contributes to the subscription line.
///
/// `user_id` is `userProfile.id` and is used only to build the legacy usage
/// query. Email, organization id, status, and feature flags are not taken.
#[derive(Clone, Debug, PartialEq)]
pub struct FactoryAuth {
    pub org: Option<String>,
    pub tier: Option<String>,
    pub plan: Option<String>,
    pub user_id: Option<String>,
}

pub fn parse_factory_auth(text: &str) -> Option<FactoryAuth> {
    let body: serde_json::Value = serde_json::from_str(text).ok()?;
    let org_obj = &body["organization"];
    let auth = FactoryAuth {
        org: non_empty(&org_obj["name"]),
        tier: non_empty(&org_obj["subscription"]["factoryTier"]),
        plan: non_empty(&org_obj["subscription"]["orbSubscription"]["plan"]["name"]),
        user_id: non_empty(&body["userProfile"]["id"]),
    };
    (auth.org.is_some() || auth.tier.is_some() || auth.plan.is_some() || auth.user_id.is_some())
        .then_some(auth)
}

/// The `FACTORY_API_KEY` line in `~/.factory/.env`.
///
/// Blank lines and `#` comments are skipped. `export` is optional. The
/// first key line wins; an empty value is not a key.
pub fn parse_factory_dotenv(text: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
        let Some(value) = line.strip_prefix("FACTORY_API_KEY=") else {
            continue;
        };
        let value = value.trim();
        let value = value
            .split_once(" #")
            .map(|(head, _)| head.trim())
            .unwrap_or(value);
        let value = quoted(value).unwrap_or(value);
        let value = value.trim();
        return (!value.is_empty()).then(|| value.to_string());
    }
    None
}

fn quoted(value: &str) -> Option<&str> {
    let bytes = value.as_bytes();
    if bytes.len() >= 2
        && (bytes[0] == b'"' || bytes[0] == b'\'')
        && bytes[0] == bytes[bytes.len() - 1]
    {
        Some(&value[1..value.len() - 1])
    } else {
        None
    }
}

fn non_empty(v: &serde_json::Value) -> Option<String> {
    let text = strip_controls(v.as_str()?.trim());
    (!text.is_empty()).then_some(text)
}

/// An ISO-8601 string, a numeric string, or a number. Above `millis_above`
/// the number is milliseconds.
fn epoch_from(v: &serde_json::Value, millis_above: f64) -> Option<f64> {
    match v {
        serde_json::Value::Number(n) => n.as_f64().and_then(|n| epoch_number(n, millis_above)),
        serde_json::Value::String(s) => {
            let s = s.trim();
            if s.is_empty() {
                return None;
            }
            if let Ok(n) = s.parse::<f64>() {
                epoch_number(n, millis_above)
            } else {
                iso_epoch(s)
            }
        }
        _ => None,
    }
}

fn epoch_number(n: f64, millis_above: f64) -> Option<f64> {
    if !n.is_finite() || n <= 0.0 {
        return None;
    }
    Some(if n > millis_above { n / 1000.0 } else { n })
}

/// A balance drawn as digits. No currency symbol: the body does not name one.
pub fn format_balance(n: f64) -> String {
    if !n.is_finite() {
        return String::new();
    }
    let cents = (n * 100.0).round() / 100.0;
    if (cents - cents.trunc()).abs() < 1e-9 {
        format!("{}", cents as i64)
    } else {
        let text = format!("{cents:.2}");
        text.trim_end_matches('0').trim_end_matches('.').to_string()
    }
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
    fn a_coderabbit_report_with_a_rolling_quota_gives_what_is_left_of_it() {
        // The 0.8 report adds the rolling allowance; its exact wording is
        // unseen, so two plausible spellings are read the same way.
        let text = "Your reviews           : 42\n\
                    Available reviews      : 3 of 5\n\
                    Rolling window         : 1 hour\n\
                    Capacity returns       : in 23m\n\
                    Period resets          : 2026-09-30\n";
        let u = parse_coderabbit_usage(text).expect("parsed");
        assert_eq!(u.available(), Some((3, Some(5))));
        assert_eq!(u.window_secs(), Some(3600.0));
        assert_eq!(u.returns_at(1000.0), Some(1000.0 + 23.0 * 60.0));
        let split = parse_coderabbit_usage(
            "Included reviews left : 0/8\nQuota window : 60 minutes\n\
             Available again : 2026-09-29T13:05:00Z\n",
        )
        .expect("parsed");
        assert_eq!(split.available(), Some((0, Some(8))));
        assert_eq!(split.window_secs(), Some(3600.0));
        assert_eq!(split.returns_at(0.0), iso_epoch("2026-09-29T13:05:00Z"));
    }

    #[test]
    fn a_coderabbit_count_with_no_limit_is_not_given_one() {
        // A limit on a line of its own is taken; a bare count stays bare.
        let apart = parse_coderabbit_usage("Available reviews : 4\nReview limit : 5 per hour\n")
            .expect("parsed");
        assert_eq!(apart.available(), Some((4, Some(5))));
        let over = parse_coderabbit_usage("Available reviews : 7 of 5\n").expect("parsed");
        assert_eq!(over.available(), Some((7, None)));
        let bare = parse_coderabbit_usage("Available reviews : 4\n").expect("parsed");
        assert_eq!(bare.available(), Some((4, None)));
        // The 0.7 report has no quota at all.
        let old = parse_coderabbit_usage("Your reviews : 25\nPeriod resets : 2026-09-30\n").unwrap();
        assert_eq!(old.available(), None);
        assert_eq!(old.window_secs(), None);
    }

    // `coderabbit usage` from CLI 0.8, captured outside a repository, with
    // the organisation and user replaced.
    const CODERABBIT_08_OUTSIDE_A_REPO: &str = "\
────────────────────────────────────────
CodeRabbit Usage

Included reviews
Availability : unavailable
Note         : Run from a git repository to check included reviews.

Billing period
Organization  : example-org
Usage billing : active
User          : example-user
Your reviews  : 94
Your spend    : $5.25
Review cap    : $40.00 per billing month (shared subscription)
Period resets : 2026-10-06
────────────────────────────────────────
";

    // The same, captured inside a repository.
    const CODERABBIT_08_IN_A_REPO: &str = "\
────────────────────────────────────────
CodeRabbit Usage

Included reviews
Repository : example-org/example-repo
Remaining  : 10 of 10
Window     : rolling 1 hour

Billing period
Organization  : example-org
Usage billing : active
User          : example-user
Your reviews  : 95
Your spend    : $5.25
Review cap    : $40.00 per billing month (shared subscription)
Period resets : 2026-10-06
────────────────────────────────────────
";

    #[test]
    fn a_captured_coderabbit_report_inside_a_repository_gives_the_allowance() {
        // `Remaining` is the count and its limit, `Window` the rolling hour;
        // nothing is spent, so CodeRabbit names no return time.
        let u = parse_coderabbit_usage(CODERABBIT_08_IN_A_REPO).expect("parsed");
        assert_eq!(u.available(), Some((10, Some(10))));
        assert_eq!(u.window_secs(), Some(3600.0));
        assert_eq!(u.returns_at(1000.0), None);
        assert_eq!(u.unavailable_why(), None);
        assert_eq!(u.reviews(), Some(95));
        assert_eq!(u.get("repository"), Some("example-org/example-repo"));
    }

    #[test]
    fn a_captured_coderabbit_report_outside_a_repository_keeps_its_billing_period() {
        // Availability is a quota line, and `unavailable` is CodeRabbit's
        // answer rather than a count, so nothing is read as reviews left.
        let u = parse_coderabbit_usage(CODERABBIT_08_OUTSIDE_A_REPO).expect("parsed");
        assert_eq!(u.reviews(), Some(94));
        assert_eq!(u.get("period resets"), Some("2026-10-06"));
        assert_eq!(u.get("your spend"), Some("$5.25"));
        assert_eq!(u.available(), None);
        assert_eq!(u.window_secs(), None);
        assert_eq!(
            u.unavailable_why().as_deref(),
            Some("Run from a git repository to check included reviews")
        );
        assert_eq!(coderabbit_quota_field("availability"), Some(QuotaField::Available));
        // Inside a repository the same line carries the count.
        let inside = parse_coderabbit_usage("Availability : 3 of 8\n").expect("parsed");
        assert_eq!(inside.available(), Some((3, Some(8))));
        assert_eq!(inside.unavailable_why(), None);
    }

    #[test]
    fn coderabbit_spans_read_words_and_short_units() {
        // Words, short units run together, and a window named without a number.
        assert_eq!(coderabbit_span_secs("1 hour"), Some(3600.0));
        assert_eq!(coderabbit_span_secs("1h5m"), Some(3900.0));
        assert_eq!(coderabbit_span_secs("rolling hour"), Some(3600.0));
        assert_eq!(coderabbit_span_secs("soon"), None);
        assert_eq!(coderabbit_quota_field("capacity returns"), Some(QuotaField::Returns));
        assert_eq!(coderabbit_quota_field("your reviews"), None);
        assert_eq!(coderabbit_quota_field("period resets"), None);
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
    fn a_notion_window_label_cannot_drive_the_terminal() {
        // The rolling window's length is drawn as its lane's label.
        let raw = r#"{"status":"within_limit",
            "window":{"window":"6\u001b]0;pwned\u0007h\n","used":1,"limit":10}}"#;
        let a = parse_notion_allowance(raw).unwrap();
        let span = a.rolling.unwrap().span;
        assert!(!span.chars().any(char::is_control), "{span:?}");
        assert!(span.starts_with("6h"), "{span:?}");
    }

    #[test]
    fn a_notion_workspace_name_cannot_drive_the_terminal() {
        // The name is drawn on the tab, and a JSON escape can carry an OSC or
        // a newline straight to the terminal.
        let raw = r#"{"u1":{"notion_user":{"u1":{"value":{"id":"u1","email":"a\u001b[2J@b.c"}}},
            "space":{"s1":{"value":{"id":"s1","name":"Ac\u001b]0;pwned\u0007me\nCo",
            "subscription_tier":"busi\u009bness"}}}}}"#;
        let (email, spaces) = parse_notion_spaces(raw).unwrap();
        assert_eq!(email, "a@b.c");
        assert_eq!(spaces[0].name, "Acme Co");
        assert!(!spaces[0].tier.chars().any(char::is_control), "{:?}", spaces[0].tier);
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
    #[test]
    fn a_devin_quota_keeps_the_percents_it_was_sent() {
        let q = parse_devin_quota(
            r#"{"daily_percentage":0.4,"weekly_percentage":42,"daily_reset_at":"2026-09-30T00:00:00Z",
                "weekly_reset_at":1700000000000,"hide_daily_quota":false,"overage_balance":12.5,
                "is_quota_plan":true,"plan":"pro_plus"}"#,
        )
        .unwrap();
        // 0.4 stays 0.4. Multiplying a sub-one percent by 100 would draw 40%.
        assert_eq!(q.daily.as_ref().map(|w| w.pct), Some(0.4));
        assert_eq!(q.weekly.as_ref().map(|w| w.pct), Some(42.0));
        assert!(q.daily.as_ref().unwrap().reset.is_some());
        // Above 10_000_000_000 the reset is milliseconds.
        assert_eq!(q.weekly.as_ref().unwrap().reset, Some(1_700_000_000.0));
        assert!(!q.daily_hidden);
        assert_eq!(q.balance, Some(12.5));
        assert_eq!(q.plan.as_deref(), Some("pro_plus"));
    }

    #[test]
    fn a_hidden_devin_daily_is_omitted_and_a_missing_balance_is_not_zero() {
        let hidden = parse_devin_quota(
            r#"{"daily_percentage":40,"weekly_percentage":0,"hide_daily_quota":true}"#,
        )
        .unwrap();
        assert!(hidden.daily.is_none());
        assert!(hidden.daily_hidden);
        // The zero was sent. It is not the stand-in for a window that was left out.
        assert_eq!(hidden.weekly.as_ref().map(|w| w.pct), Some(0.0));
        assert_eq!(hidden.balance, None);
        // Only a JSON boolean true hides the daily window.
        let kept = parse_devin_quota(
            r#"{"daily_percentage":40,"weekly_percentage":1,"hide_daily_quota":"true"}"#,
        )
        .unwrap();
        assert_eq!(kept.daily.as_ref().map(|w| w.pct), Some(40.0));
        let cents =
            parse_devin_quota(r#"{"weekly_percentage":1,"overage_balance_cents":250}"#).unwrap();
        assert!(cents.daily.is_none() && !cents.daily_hidden);
        assert_eq!(cents.balance, Some(2.5));
        assert!(parse_devin_quota(r#"{"hide_daily_quota":true,"daily_percentage":40}"#).is_none());
    }

    #[test]
    fn a_devin_nested_window_is_used_only_when_the_top_level_percent_is_absent() {
        let q = parse_devin_quota(
            r#"{"quota_usage":{"daily_quota":{"used":3,"limit":10,"daily_reset_at":1700000000},
                "weekly_quota":{"remaining_percent":25},"plan_name":"nested"}}"#,
        )
        .unwrap();
        assert_eq!(q.daily.as_ref().map(|w| w.pct), Some(30.0));
        assert_eq!(q.daily.as_ref().unwrap().reset, Some(1_700_000_000.0));
        assert_eq!(q.weekly.as_ref().map(|w| w.pct), Some(75.0));
        // The plan key has to be at the top. A nested one is a different object.
        assert!(q.plan.is_none());
        let top = parse_devin_quota(
            r#"{"daily_percentage":8,"quota_usage":{"daily_quota":{"used_percent":90}}}"#,
        )
        .unwrap();
        assert_eq!(top.daily.as_ref().map(|w| w.pct), Some(8.0));
        assert!(top.weekly.is_none());
    }

    #[test]
    fn factory_rate_limits_keep_each_standard_window_and_skip_an_empty_core() {
        let body = r#"{"usesTokenRateLimitsBilling":true,"extraUsageBalanceCents":0,
            "overagePreference":"on_demand",
            "limits":{"standard":{
                "fiveHour":{"usedPercent":10,"secondsRemaining":1000},
                "weekly":{"usedPercent":20,"windowEnd":1700000000000},
                "monthly":{"usedPercent":0,"windowEnd":1600000000}}}}"#;
        let b = parse_factory_billing_limits(body).unwrap();
        assert_eq!(b.standard.five_hour.as_ref().map(|w| w.pct), Some(10.0));
        assert_eq!(
            b.standard.five_hour.as_ref().unwrap().seconds_remaining,
            Some(1000.0)
        );
        assert_eq!(
            b.standard.weekly.as_ref().unwrap().window_end,
            Some(1_700_000_000.0)
        );
        // A past windowEnd is kept. The percent sent beside it stays 0, which was sent.
        assert_eq!(
            b.standard.monthly.as_ref().map(|w| (w.pct, w.window_end)),
            Some((0.0, Some(1_600_000_000.0)))
        );
        assert!(b.core.is_none());
        assert_eq!(b.balance, Some(0.0));
        assert_eq!(b.overage_preference.as_deref(), Some("on_demand"));
        let empty_core = r#"{"usesTokenRateLimitsBilling":true,"limits":{"standard":{
            "fiveHour":{"usedPercent":1},"weekly":{"usedPercent":1},"monthly":{"usedPercent":1}},
            "core":{"fiveHour":{"usedPercent":0},"weekly":{"usedPercent":0},"monthly":{"usedPercent":0}}}}"#;
        assert!(parse_factory_billing_limits(empty_core)
            .unwrap()
            .core
            .is_none());
        let partial_core = r#"{"usesTokenRateLimitsBilling":true,"limits":{"standard":{
            "fiveHour":{"usedPercent":1},"weekly":{"usedPercent":1},"monthly":{"usedPercent":1}},
            "core":{"weekly":{"usedPercent":4,"secondsRemaining":50}}}}"#;
        let core = parse_factory_billing_limits(partial_core)
            .unwrap()
            .core
            .unwrap();
        assert!(core.five_hour.is_none() && core.monthly.is_none());
        assert_eq!(core.weekly.as_ref().map(|w| w.pct), Some(4.0));
        // A bare 0% has none of the fields the empty-pool rule treats as
        // data, so a neighbouring window does not turn it into a bar.
        let bare_zero = r#"{"usesTokenRateLimitsBilling":true,"limits":{"standard":{
            "fiveHour":{"usedPercent":1},"weekly":{"usedPercent":1},"monthly":{"usedPercent":1}},
            "core":{"fiveHour":{"usedPercent":0},"weekly":{"usedPercent":4,"secondsRemaining":50}}}}"#;
        let beside = parse_factory_billing_limits(bare_zero)
            .unwrap()
            .core
            .unwrap();
        assert!(beside.five_hour.is_none());
        assert_eq!(beside.weekly.as_ref().map(|w| w.pct), Some(4.0));
        // A 0% sent with a windowEnd is a real reading. The date is what
        // makes the window data, and the percent stays 0.
        let dated_zero = r#"{"usesTokenRateLimitsBilling":true,"limits":{"standard":{
            "fiveHour":{"usedPercent":1},"weekly":{"usedPercent":1},"monthly":{"usedPercent":1}},
            "core":{"fiveHour":{"usedPercent":0,"windowEnd":4102444800000},"weekly":{"usedPercent":4}}}}"#;
        let dated = parse_factory_billing_limits(dated_zero)
            .unwrap()
            .core
            .unwrap();
        assert_eq!(dated.five_hour.as_ref().map(|w| w.pct), Some(0.0));
        assert_eq!(dated.weekly.as_ref().map(|w| w.pct), Some(4.0));
        // A 0% whose only extra field is a non-positive secondsRemaining
        // has no reset once that field is dropped, so it is not a bar.
        for seconds in ["0", "-5"] {
            let dead = format!(
                r#"{{"usesTokenRateLimitsBilling":true,"limits":{{"standard":{{
                "fiveHour":{{"usedPercent":1}},"weekly":{{"usedPercent":1}},"monthly":{{"usedPercent":1}}}},
                "core":{{"fiveHour":{{"usedPercent":0,"secondsRemaining":{seconds}}},"weekly":{{"usedPercent":4}}}}}}}}"#
            );
            let core = parse_factory_billing_limits(&dead).unwrap().core.unwrap();
            assert!(core.five_hour.is_none(), "secondsRemaining {seconds}");
            assert_eq!(core.weekly.as_ref().map(|w| w.pct), Some(4.0));
        }
        let counting = r#"{"usesTokenRateLimitsBilling":true,"limits":{"standard":{
            "fiveHour":{"usedPercent":1},"weekly":{"usedPercent":1},"monthly":{"usedPercent":1}},
            "core":{"fiveHour":{"usedPercent":0,"secondsRemaining":50}}}}"#;
        let live = parse_factory_billing_limits(counting)
            .unwrap()
            .core
            .unwrap();
        assert_eq!(
            live.five_hour
                .as_ref()
                .map(|w| (w.pct, w.seconds_remaining)),
            Some((0.0, Some(50.0)))
        );
        let partial = parse_factory_billing_limits(
            r#"{"usesTokenRateLimitsBilling":true,"limits":{"standard":{"fiveHour":{"usedPercent":1},"weekly":{"usedPercent":2}}}}"#,
        )
        .unwrap();
        assert_eq!(
            partial.standard.five_hour.as_ref().map(|w| w.pct),
            Some(1.0)
        );
        assert_eq!(partial.standard.weekly.as_ref().map(|w| w.pct), Some(2.0));
        assert!(partial.standard.monthly.is_none());
        assert!(parse_factory_billing_limits(
            r#"{"usesTokenRateLimitsBilling":true,"limits":{"standard":{"fiveHour":{},"weekly":{"secondsRemaining":1}}}}"#
        )
        .is_none());
        assert!(parse_factory_billing_limits(
            r#"{"usesTokenRateLimitsBilling":false,"limits":{"standard":{"fiveHour":{"usedPercent":1},"weekly":{"usedPercent":1},"monthly":{"usedPercent":1}}}}"#
        )
        .is_none());
        let no_cents = r#"{"usesTokenRateLimitsBilling":true,"limits":{"standard":{
            "fiveHour":{"usedPercent":1},"weekly":{"usedPercent":1},"monthly":{"usedPercent":1}}}}"#;
        assert_eq!(
            parse_factory_billing_limits(no_cents).unwrap().balance,
            None
        );
    }

    #[test]
    fn a_legacy_factory_percent_comes_from_the_ratio_or_not_at_all() {
        let usage = parse_factory_usage(
            r#"{"usage":{"endDate":1700000000000,
                "standard":{"userTokens":10,"totalAllowance":100,"usedRatio":0.10,"orgTotalTokensUsed":40},
                "premium":{"userTokens":5,"totalAllowance":100,"usedRatio":0}}}"#,
        )
        .unwrap();
        assert_eq!(usage.end, Some(1_700_000_000.0));
        assert_eq!(usage.standard.pct, Some(10.0));
        assert_eq!(usage.standard.org_tokens, Some(40.0));
        // A zero ratio with tokens already spent falls through to the counts.
        let spent = parse_factory_usage(
            r#"{"usage":{"standard":{"userTokens":25,"totalAllowance":100,"usedRatio":0}}}"#,
        )
        .unwrap();
        assert_eq!(spent.standard.pct, Some(25.0));
        assert!(spent.premium.pct.is_none());
        assert_eq!(spent.premium.org_tokens, None);
        let missing = parse_factory_usage(r#"{"usage":{"standard":{"userTokens":25}}}"#).unwrap();
        assert!(missing.standard.pct.is_none());
        assert!(!missing.standard.unlimited);
        let huge = parse_factory_usage(
            r#"{"usage":{"standard":{"userTokens":10,"totalAllowance":1000000000001,"usedRatio":0.5}}}"#,
        )
        .unwrap();
        assert!(huge.standard.pct.is_none() && huge.standard.unlimited);
        // Outside 0–1 the ratio is not read as an already-percent.
        let odd = parse_factory_usage(r#"{"usage":{"premium":{"usedRatio":40}}}"#).unwrap();
        assert!(odd.premium.pct.is_none());
        assert!(parse_factory_usage(r#"{"limits":{}}"#).is_none());
    }

    #[test]
    fn factory_auth_keeps_the_labels_and_the_user_id_and_drops_the_rest() {
        let auth = parse_factory_auth(
            r#"{"organization":{"id":"org_1","name":"Acme","subscription":{"factoryTier":"pro",
                "orbSubscription":{"status":"active","plan":{"id":"plan_1","name":"Factory Pro"}}}},
                "userProfile":{"id":"user-1","email":"a@b.c"},"featureFlags":{"x":true}}"#,
        )
        .unwrap();
        assert_eq!(auth.org.as_deref(), Some("Acme"));
        assert_eq!(auth.tier.as_deref(), Some("pro"));
        assert_eq!(auth.plan.as_deref(), Some("Factory Pro"));
        assert_eq!(auth.user_id.as_deref(), Some("user-1"));
        assert!(parse_factory_auth(r#"{"featureFlags":{}}"#).is_none());
    }

    #[test]
    fn a_factory_dotenv_line_is_the_key_and_nothing_else_in_the_file() {
        assert_eq!(
            parse_factory_dotenv("# comment\n\nexport FACTORY_API_KEY=\"abc\"\nOTHER=1\n")
                .as_deref(),
            Some("abc")
        );
        assert_eq!(
            parse_factory_dotenv("FACTORY_API_KEY='xyz' # trailing\n").as_deref(),
            Some("xyz")
        );
        assert_eq!(parse_factory_dotenv("FACTORY_API_KEY=\n").as_deref(), None);
        assert_eq!(
            parse_factory_dotenv("# FACTORY_API_KEY=nope\n").as_deref(),
            None
        );
        assert_eq!(format_balance(2.5), "2.5");
        assert_eq!(format_balance(0.0), "0");
        assert!(!format_balance(12.5).contains('$'));
    }
}
