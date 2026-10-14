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

//! Cursor: the three lanes its own Usage view shows, the spend its server
//! prices, and the tracking database that says how much code it wrote.
//!
//! Metered usage and API-rate estimates share one dashboard event window.
//! Missing prices remain visible, not zero. The authorship
//! counts answer a different question - how much code the agent wrote,
//! not what it cost - and the tab keeps the two apart.

use std::collections::HashMap;

use chrono::{Datelike, Duration as Days, Local, NaiveDate, TimeZone};
use rusqlite::{Connection, OpenFlags};
use opscope_core as tc;

use crate::shared::*;
use crate::*;

/// The Connect endpoint cursor-agent's own Usage view calls. Not the
/// documented cursor.com dashboard API - that one wants a browser cookie
/// and returns 401 to anything this machine has. The CLI talks to
/// aiserver.v1.DashboardService with the bearer token it stores beside its
/// config, which is the credential this reuses.
const CURSOR_RPC: &str = "https://api2.cursor.sh/aiserver.v1.DashboardService/";

/// The lanes in the order cursor-agent's Usage view lists them: each one's
/// label, the field it reads, and how far up Cursor's own colour it sits.
///
/// One tint per lane rather than three unrelated hues. These are
/// categories, not one gauge, so a green-to-red severity ramp would imply
/// a relationship between them that does not exist - and each bar is
/// labelled and carries its own percentage, so the colour is decoration.
/// Tints of Cursor's own colour stay distinguishable while reading as
/// Cursor's, which three borrowed colours never did.
const CURSOR_LANES: &[(&str, &str, f64)] = &[
    ("total", "totalPercentUsed", 1.0),
    ("cursor models", "autoPercentUsed", 0.80),
    ("other models", "apiPercentUsed", 0.62),
];

/// Grok Bot's weekly included allowance, which Cursor's own API calls
/// "sand" - the name that shows up in the spend breakdown as
/// `sand-default` and `sand-automation`.
///
/// It is not one of the three plan lanes and does not share their window:
/// the plan lanes run to the monthly billing cycle, this one resets
/// weekly on a date the response states. Drawing it against the cycle's
/// clock would put its pace marker in the wrong place every time.
///
/// Reached through the same RPC service and the same bearer token as the
/// three plan lanes. Cursor's own dashboard calls this over the website as
/// `POST /api/dashboard/get-sand-usage-status`, which is cookie-only and
/// answers the app's token with a redirect to the login provider - so the
/// obvious reading is that this needs a browser session. It does not: the
/// Connect service exposes the same call, and the token already on disk is
/// enough. Worth stating, because the website's route is the documented
/// one and following it would have put a credential in config.json for no
/// reason at all.
const SAND_METHOD: &str = "GetSandUsageStatus";
const SAND_KEY: &str = "cursor:sand";
/// The allowance is weekly, so this need not be brisk.
const SAND_TTL: f64 = 900.0;

/// The weekly included allowance's name, on `[+]` and on the tab alike.
/// Shared by Cursor, Grok Bot and the Grok CLI, so `included` alone left
/// out the part a reader most needs: that it is one allowance, by the week.
pub const POOL_LABEL: &str = "weekly pool";

/// The prepaid balance's bar: what has been drawn from it against what was
/// paid in. X calls the same balance "Additional usage credits".
const PREPAID_LABEL: &str = "prepaid credits";

/// This month's additional usage against the monthly limit set on
/// cursor.com - its "Monthly additional usage limit". The summary adds the
/// limit's size, since a percentage of an unnamed ceiling is not a number
/// anyone can act on.
const LIMIT_LABEL: &str = "usage limit";

/// What GetHardLimit answers when no limit is set: INT32_MAX dollars.
///
/// Measured, not guessed. The same account answered `{"hardLimit":
/// 2147483647}` before a monthly limit was set on cursor.com and
/// `{"hardLimit": 100}` a minute after it was set to $100. Read as a cap it
/// drew `$0.00 of $2147483647.00 limit`, which is a sentinel shown as money.
const NO_LIMIT: f64 = 2_147_483_647.0;

/// The events RPC's own ceiling per request.
const EVENT_PAGE: usize = 1000;
/// CodexBar's safety ceiling; reaching it is an error, not a partial total.
const EVENT_PAGES: usize = 200;
/// Full history paging is too slow to repeat on a redraw.
const EVENTS_TTL: f64 = 1800.0;

/// What Cursor publishes, and what its tracking database recorded here.
#[derive(Clone, Default)]
pub struct Data {
    /// The tracking database was readable; the counts below mean something.
    ok: bool,
    why: String,
    /// Absent rather than broken, so the fix is running the agent.
    db_missing: bool,
    /// GetCurrentPeriodUsage: the three lanes and the billing cycle.
    live: Option<serde_json::Value>,
    /// Why there is no live reading, when there is none. Empty while the
    /// endpoint is answering, or when nothing has been asked yet.
    live_why: String,
    /// GetPlanInfo: the plan the percentages are percentages of.
    plan: Option<serde_json::Value>,
    /// GetHardLimit: the extra-usage cap in dollars, and - on the
    /// dashboard's own version of the call - whether extra usage is
    /// allowed at all. The second opinion that tells an absent cap from a
    /// forbidden one.
    hard: Option<serde_json::Value>,
    /// GetUserPrepaidBilling: the prepaid credit balance a unified plan
    /// bills additional usage against. Asked only on a unified plan - an
    /// account on its own plan has no balance, and asking would spend a
    /// request on every refresh to be told so.
    prepaid: Option<serde_json::Value>,
    /// GetMonthlyBillingCycle: the monthly cycle the additional-usage limit
    /// resets on. Asked only on a unified plan, where the usage call's own
    /// cycle has stopped being monthly and become the weekly included pool.
    month: Option<serde_json::Value>,
    /// The profile's activity summary - the daily token counts behind the
    /// heatmap cursor.com/@handle and grok.com both show. Asked only on a
    /// unified plan.
    activity: Option<serde_json::Value>,
    /// Where this account stands relative to the unified plan, once both
    /// vendors have been read. Set by `vendors::link_spacexsi`.
    pub spacexsi: Spacexsi,
    /// The pool's split by product - display name and whole percent of the
    /// week - from Grok, when Grok is on the same account. Cursor's own calls
    /// split it only into auto and API, and API is nought on this plan.
    /// Set with `spacexsi`.
    pub pool_split: Vec<(String, f64)>,
    /// Grok's figure for the same pool, when Grok is on the same account
    /// and current - drawn only when Cursor answers without a pool figure
    /// of its own, so the tab shows the reading there is rather than
    /// none. Set with `spacexsi`.
    pub pool_from_grok: Option<f64>,
    /// The per-day summary folded out of GetFilteredUsageEvents.
    events: Option<serde_json::Value>,
    /// GetAggregatedUsageEvents: per-model cents over the window.
    spend: Option<serde_json::Value>,
    /// get-sand-usage-status: the Grok Bot weekly allowance, when a cookie
    /// was configured and the account has one.
    sand: Option<serde_json::Value>,
    /// Why it is not showing, when a cookie was configured and it still is
    /// not. Empty when none was configured - that is not a failure.
    sand_why: String,
    hashes: i64,
    conversations: i64,
    models: i64,
    by_model: Vec<(String, i64)>,
    commits: i64,
    lines: i64,
    human_lines: i64,
    /// Milliseconds, from the newest tracked edit.
    last: Option<f64>,
}

/// A number whether it arrived as one or as a string.
///
/// Connect writes int64 as JSON strings, so reading the billing cycle's
/// timestamps with a plain as_f64 would produce a wrong window with no
/// error - the kind of silent zero this widget exists to avoid.
fn loose(v: &serde_json::Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str()?.trim().parse().ok())
}

/// Cents as dollars. Every money field on this service is cents, and the
/// one that is not - GetHardLimit's `hardLimit` - is turned into cents
/// before it reaches here.
fn money(cents: f64) -> String {
    format!("${:.2}", cents / 100.0)
}

/// A billing-cycle timestamp as the day it falls on, in this machine's zone.
///
/// A date rather than a countdown, because this sits at the end of a
/// sentence about money, and "resets 8 Oct" is something a reader can hold
/// against a statement in a way "in 22h" is not.
fn day_month(ms: f64) -> String {
    match Local.timestamp_millis_opt(ms as i64).single() {
        Some(at) => format!("{} {}", at.day(), MONTHS[at.month0() as usize]),
        None => String::new(),
    }
}

/// Thousands separators: "1,482,113 lines" is readable, "1482113" is not.
fn commas(n: i64) -> String {
    let digits = n.abs().to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    if n < 0 {
        format!("-{}", out)
    } else {
        out
    }
}

/// Which account the token on disk belongs to, as a short key for the
/// caches holding that account's money and plan.
///
/// The refresh token, hashed: it changes when someone signs in as another
/// account, so a balance read for one account is never served for another,
/// and it is never written anywhere itself. A refresh that rotates it only
/// costs one early re-read.
pub fn account_key() -> String {
    use std::hash::{Hash, Hasher};
    let auth = read_json(&under_home(".config/cursor/auth.json")).unwrap_or_default();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    match text(&auth, "refreshToken") {
        t if !t.is_empty() => t.hash(&mut h),
        _ => text(&auth, "accessToken").hash(&mut h),
    }
    format!("{:x}", h.finish())
}

/// Whether Cursor's usage call answered this pass - the pool's live figure.
pub fn has_live(d: &Data) -> bool {
    d.live.is_some()
}

fn cursor_token() -> Option<String> {
    let auth = read_json(&under_home(".config/cursor/auth.json"))?;
    let tok = text(&auth, "accessToken");
    if tok.is_empty() {
        None
    } else {
        Some(tok)
    }
}

/// One Connect-style RPC to the dashboard service.
///
/// Undocumented and versioned only by the CLI bundle it was read out of,
/// so every failure is silent and the tab simply falls back to whatever
/// else it has. The bearer token rides in a header that post_json feeds to
/// curl on its standard input, never in an argument - /proc/<pid>/cmdline
/// is world-readable.
fn cursor_rpc(method: &str, body: &serde_json::Value) -> Option<serde_json::Value> {
    let tok = cursor_token()?;
    let bearer = format!("Bearer {}", tok);
    post_json(
        &format!("{}{}", CURSOR_RPC, method),
        &[
            ("Authorization", bearer.as_str()),
            ("Content-Type", "application/json"),
            ("Connect-Protocol-Version", "1"),
            ("User-Agent", "opscope"),
        ],
        &body.to_string(),
        25,
    )
}

/// Plan usage, from the endpoint cursor-agent's own Usage view calls.
///
/// A missing token is a local fact and rides in the cached value, so the
/// summary can name it for as long as the absence lasts. A silent endpoint
/// is a refusal: it stays `None` so `cached` backs off rather than treating
/// the diagnostic as a successful reading.
fn cursor_live() -> Option<serde_json::Value> {
    if cursor_token().is_none() {
        return Some(serde_json::json!({"why": "no token - Cursor has not signed in here"}));
    }
    cursor_rpc("GetCurrentPeriodUsage", &serde_json::json!({}))
        .map(|u| serde_json::json!({"u": u}))
}

/// Which Cursor plan the percentages are percentages of.
///
/// GetPlanInfo is where the plan's name and price live - the usage call
/// carries neither, and its $400 limit is meaningless without knowing that
/// is what Ultra includes.
fn cursor_plan() -> Option<serde_json::Value> {
    cursor_rpc("GetPlanInfo", &serde_json::json!({}))
}

/// The Grok Bot allowance, or why it could not be read.
///
/// Best-effort by contract: every failure here has to leave Cursor's three
/// monthly bars exactly as they were, because this is an extra lane and not
/// a precondition for the rest of the tab.
fn sand_status() -> Result<serde_json::Value, String> {
    let tok = cursor_token().ok_or("no Cursor token on this disk")?;
    let bearer = format!("Bearer {}", tok);
    post_json_said(
        &format!("{}{}", CURSOR_RPC, SAND_METHOD),
        &[
            ("Authorization", bearer.as_str()),
            ("Content-Type", "application/json"),
            ("Connect-Protocol-Version", "1"),
            ("User-Agent", "opscope"),
        ],
        "{}",
        20,
    )
}

/// The percentage, the window it covers, and when it resets - or nothing.
///
/// Nothing is the right answer in two different situations, and neither is
/// an error: an account with no Bot allowance at all, and a response that
/// carries the flag but no percentage. Cursor states the first as
/// `hasNonZeroIncludedLimit`, and drawing a 0% bar for an account that was
/// never given the allowance would invent a limit that does not exist.
fn sand_lane(v: &serde_json::Value) -> Option<(f64, Option<f64>, Option<f64>)> {
    if v["hasNonZeroIncludedLimit"].as_bool() != Some(true) {
        return None;
    }
    let pct = loose(&v["usagePercent"])?;
    let start = iso_epoch(&text(v, "currentPeriodStart"));
    let reset = iso_epoch(&text(v, "nextResetTimestampUtc"));
    // The window only means anything as a pair, and only forwards.
    let secs = match (start, reset) {
        (Some(s), Some(e)) if e > s => Some(e - s),
        _ => None,
    };
    Some((pct.clamp(0.0, 100.0), secs, reset))
}

/// The extra-usage cap Cursor's dashboard reports, in dollars.
///
/// `GetCurrentPeriodUsage` states a cap only when one is set, and proto3
/// omits a field sitting at its default - so an account with no cap and an
/// account forbidden from spending beyond the plan arrive there looking
/// identical. This call is the second opinion that tells them apart. It
/// answered `{"hardLimit": 50}` on the measured account, the same $50 the
/// usage block states in cents, and cursor.com's own version of it carries
/// `noUsageBasedAllowed` and `hardLimitPerUser` beside that.
///
/// Best-effort, like the Bot allowance: the cap in the usage block is
/// enough on its own for the state this machine is actually in, so a
/// refusal here must leave everything else exactly as it was.
fn cursor_hard_limit() -> Option<serde_json::Value> {
    cursor_rpc("GetHardLimit", &serde_json::json!({}))
}

/// What Cursor's monthly spend limit is set to, and what has gone against
/// it this cycle. Cents throughout, because that is what the API speaks.
///
/// Extra usage is on-demand spend past the plan's included amount: real
/// money, billed, and capped by a limit the account sets on cursor.com.
/// That setting has three states there and can be changed mid-cycle, so
/// all three have to be drawable from whatever the next response says -
/// plus a fourth for a response this parser does not recognise, which is
/// never allowed to masquerade as one of the other three.
#[derive(Debug, Clone, PartialEq)]
enum ExtraState {
    /// A dollar cap. `used` may exceed `limit`: lowering the cap below what
    /// has already been spent is something the account can do at any time,
    /// and the figures then have to stay the real ones.
    Fixed {
        used: f64,
        limit: f64,
        /// A shared team budget rather than this account's own cap.
        shared: bool,
    },
    /// Extra usage allowed with no ceiling. A percentage of nothing, so no
    /// bar - the refusal `sand_lane` already makes for an allowance that
    /// does not exist.
    Unlimited { used: f64 },
    /// No extra usage allowed. Anything spent before it was turned off is
    /// still billable and stays on screen.
    Disabled { used: f64 },
    /// No spend-limit block at all, or a shape this parser has not seen.
    /// The keys it did see ride along, so the tab can name them instead of
    /// reading a state into them.
    NotReported { keys: Vec<String> },
}

/// Which state the spend limit is in, from the usage block and the hard
/// limit beside it.
///
/// Only the fixed state has been measured on a real account. The other two
/// rest on assumptions, each named in the test that covers it: proto3 omits
/// a field at its default, so a cap that is off and a cap that was never
/// set both arrive with no `individualLimit`, and `noUsageBasedAllowed` -
/// which cursor.com's dashboard carries beside `hardLimit` - is the only
/// field that can say extra usage is forbidden rather than merely uncapped.
/// Anything matching none of the three is `NotReported` carrying its keys,
/// because an admitted gap is worth more than a guessed state.
fn extra_state(live: &serde_json::Value, hard: Option<&serde_json::Value>) -> ExtraState {
    let nothing = serde_json::Value::Null;
    let hard = hard.unwrap_or(&nothing);
    let Some(blk) = live.get("spendLimitUsage").filter(|v| v.is_object()) else {
        return ExtraState::NotReported { keys: Vec::new() };
    };
    // An absent `limitType` is proto3's empty string, which is no
    // evidence of a pool. Only a stated type other than `user` is.
    let shared = !matches!(text(blk, "limitType").as_str(), "" | "user");
    // A user cap is this account's spend; a shared pool is the pool's.
    // `individualUsed` against a team ceiling is a partial presented as a
    // total. Each is the other's fallback: proto3 omits a field at zero,
    // and on the measured user account the two figures were the same.
    let used = if shared {
        loose(&blk["totalSpend"]).or_else(|| loose(&blk["individualUsed"]))
    } else {
        loose(&blk["individualUsed"]).or_else(|| loose(&blk["totalSpend"]))
    };
    if hard["noUsageBasedAllowed"].as_bool() == Some(true) {
        return ExtraState::Disabled { used: used.unwrap_or(0.0) };
    }
    // INT32_MAX is how GetHardLimit says no limit is set, so it is the
    // answer to "is there a cap" rather than the size of one.
    let no_limit = loose(&hard["hardLimit"]).is_some_and(|d| d >= NO_LIMIT);
    // Cents in the usage block, dollars in the hard limit. The two agreed
    // at $50 on the measured account, and confusing them is a hundredfold
    // error in a figure about money.
    let capped = loose(&blk["individualLimit"]).filter(|l| *l > 0.0).or_else(|| {
        loose(&hard["hardLimit"])
            .or_else(|| loose(&hard["hardLimitPerUser"]))
            .filter(|d| *d > 0.0 && *d < NO_LIMIT)
            .map(|d| d * 100.0)
    });
    if let Some(limit) = capped {
        return ExtraState::Fixed {
            // A block carrying a cap and no spend is proto3 omitting a
            // zero, not a spend nobody knows - Cursor answered the call.
            used: used.unwrap_or(0.0),
            limit,
            shared,
        };
    }
    // Cursor said outright that no limit is set. No spend beside it is
    // proto3 omitting a zero, the same reading a stated cap gets above.
    if no_limit {
        return ExtraState::Unlimited { used: used.unwrap_or(0.0) };
    }
    match used {
        // A spend, no cap from either call, and nothing saying extra usage
        // is forbidden: uncapped. Assumed, not measured.
        Some(used) => ExtraState::Unlimited { used },
        None => ExtraState::NotReported {
            keys: blk.as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default(),
        },
    }
}

/// The percentage the summary draws for extra usage, where there is one.
///
/// Only the fixed state has a ceiling to be a percentage of, and the figure
/// is deliberately not clamped: a cap lowered below what is already spent
/// is a real number over 100, and the summary draws a full bar and the true
/// figure for it.
fn extra_lane_pct(state: &ExtraState) -> Option<f64> {
    match state {
        ExtraState::Fixed { used, limit, .. } if *limit > 0.0 => Some(100.0 * used / limit),
        _ => None,
    }
}

/// The two dollar lines under the bars: what the plan includes, and what
/// has been spent beyond it.
///
/// Deliberately dollars rather than two more bars. Both are spend against a
/// denominator of their own - the plan's included amount, and the account's
/// own monthly cap - and neither is the population the three percentages
/// above are percentages of, so putting them on that scale would invite
/// reading 12% and 2% as the same kind of number.
///
/// The plan pair is `includedSpend` of `limit`, not `totalSpend`:
/// `totalSpend` carries `bonusSpend` beside it, which is spend Cursor
/// granted past the included amount and is not against the limit at all.
/// Read the other way it said `$1794.80 of $400.00` on the measured
/// account, which is not a thing that can be true.
fn spend_rows(live: &serde_json::Value, hard: Option<&serde_json::Value>, w: usize, p: &Palette) -> Vec<String> {
    let plan = &live["planUsage"];
    let state = extra_state(live, hard);
    // A stated limit with no `includedSpend` is proto3 omitting a zero,
    // not a plan nobody read - the same reading `individualLimit` gets.
    let plan_pair = loose(&plan["limit"])
        .filter(|v| *v != 0.0)
        .map(|limit| (limit, loose(&plan["includedSpend"]).unwrap_or(0.0)));
    // One column for both labels, so the two amounts line up under each
    // other and the pair reads as a pair.
    let label_w = ["spend", "extra usage"].iter().map(|l| l.chars().count()).max().unwrap();
    let extra_used = match &state {
        ExtraState::Fixed { used, .. }
        | ExtraState::Unlimited { used }
        | ExtraState::Disabled { used } => Some(*used),
        ExtraState::NotReported { .. } => None,
    };
    // Right-aligned money: $9.64 under $400.00 keeps the decimal points in
    // one column, and the `of` after them too.
    let amt_w = plan_pair
        .map(|(_, inc)| money(inc).chars().count())
        .unwrap_or(0)
        .max(extra_used.map(|u| money(u).chars().count()).unwrap_or(0));
    let lbl = |name: &str| format!("  {} ", tc::pad(name, label_w));
    let mut rows: Vec<String> = Vec::new();
    if let Some((limit, included)) = plan_pair {
        rows.push(tc::seg(
            &[
                (p.dim.as_str(), lbl("spend")),
                (p.txt.as_str(), format!("{:>1$}", money(included), amt_w)),
                (p.dim.as_str(), " of ".into()),
                (p.txt.as_str(), money(limit)),
                (p.dim.as_str(), " included".into()),
                // Only when the server states one. `remainingBonus` was
                // false on the measured account and no `remaining` came
                // with it, and a remainder worked out from a pair that
                // already states itself is a third number saying nothing.
                (
                    p.dim.as_str(),
                    match loose(&plan["remaining"]).filter(|v| *v >= 0.0) {
                        Some(v) => format!(" · {} left", money(v)),
                        None => String::new(),
                    },
                ),
            ],
            w - 1,
        ));
    }
    let ends = loose(&live["billingCycleEnd"]);
    rows.push(extra_row(&state, "extra usage", ends, label_w, amt_w, w, p));
    rows
}

/// The extra-usage line, in whichever of the four states the account is in.
///
/// Every state says the same two things: what has been spent, and what is
/// allowed. An absent limit is not zero and a forbidden limit is not an
/// absent one, so the four read differently on purpose - `$0.00 of $0.00`
/// for any of them would be a cap of nothing rather than no cap.
///
/// `ends` is when the limit resets, in epoch milliseconds. On an account's
/// own plan that is the usage call's billing cycle; on the unified plan the
/// usage call's cycle is the weekly included pool, and the limit resets
/// monthly, so the caller has to say which clock it is on.
fn extra_row(
    state: &ExtraState,
    label: &str,
    ends: Option<f64>,
    label_w: usize,
    amt_w: usize,
    w: usize,
    p: &Palette,
) -> String {
    let lbl = format!("  {} ", tc::pad(label, label_w));
    let amt = |cents: f64| format!("{:>1$}", money(cents), amt_w);
    let mut line: Vec<(String, String)> = vec![(p.dim.clone(), lbl)];
    // What the row can do without on a narrow pane, in the order it gives
    // them up. Both are derivable from the figures already on the line, and
    // a row wider than its pane is worse than one that ends sooner.
    let mut tails: Vec<String> = Vec::new();
    match state {
        ExtraState::Fixed { used, limit, shared } => {
            line.push((p.txt.clone(), amt(*used)));
            line.push((p.dim.clone(), " of ".into()));
            line.push((p.txt.clone(), money(*limit)));
            line.push((
                p.dim.clone(),
                if *shared { " team pool".into() } else { " limit".to_string() },
            ));
            // Over the cap states the overage, never a negative remainder:
            // "-$9.00 left" is arithmetic where a reader needs a fact. And
            // the overage is not droppable - it is the one thing on this
            // row that is news.
            if used > limit {
                line.push((p.bad.clone(), format!(" · {} over", money(used - limit))));
            } else {
                tails.push(format!(" · {} left", money(limit - used)));
            }
        }
        // No denominator, because there is none. The dollars are still real.
        ExtraState::Unlimited { used } => {
            line.push((p.txt.clone(), amt(*used)));
            line.push((p.dim.clone(), " · no limit".into()));
        }
        // Spend already made under a limit since switched off is billable
        // and stays on screen; with nothing spent there is no figure to
        // show and the state is the whole of it.
        ExtraState::Disabled { used } => {
            if *used > 0.0 {
                line.push((p.txt.clone(), amt(*used)));
                line.push((p.dim.clone(), " · disabled".into()));
            } else {
                line.push((p.dim.clone(), " disabled".into()));
            }
        }
        ExtraState::NotReported { keys } => {
            line.push((p.warn.clone(), " not reported".into()));
            // The keys that did arrive, so an unrecognised shape can be
            // read off the pane and mapped rather than guessed at here.
            if !keys.is_empty() {
                line.push((p.dim.clone(), format!(" · keys: {}", keys.join(", "))));
            }
        }
    }
    // The cycle end belongs to every state that carries a figure, and is
    // the first thing dropped when the pane is too narrow for all of it -
    // a date is worth less than the money it is a date for.
    let when = ends.map(day_month).unwrap_or_default();
    if !when.is_empty() && !matches!(state, ExtraState::NotReported { .. }) {
        tails.push(format!(" · resets {}", when));
    }
    let mut room: usize = w.saturating_sub(1);
    room = room.saturating_sub(line.iter().map(|(_, t)| t.chars().count()).sum::<usize>());
    for tail in tails {
        let n = tail.chars().count();
        if n <= room {
            room -= n;
            line.push((p.dim.clone(), tail));
        }
    }
    let refs: Vec<(&str, String)> = line.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
    tc::seg(&refs, w - 1)
}

/// Whether GetPlanInfo says this account is on the unified plan.
///
/// Read from the plan rather than from the usage call, because the usage
/// call's shape barely moved: the same three percentages and the same
/// cycle fields, with the cycle silently shortened to a week. GetPlanInfo
/// states it twice - `isMerged` beside the plan, and an
/// `includedUsagePeriod` an account on its own plan does not carry - and
/// either is enough, so one being dropped later does not put a unified
/// account back on the monthly drawing.
fn is_unified(plan: Option<&serde_json::Value>) -> bool {
    let Some(info) = plan else {
        return false;
    };
    info["isMerged"].as_bool() == Some(true)
        || !text(&info["planInfo"], "includedUsagePeriod").is_empty()
}

/// Whether this account is on the unified Cursor and Grok plan.
pub fn unified(d: &Data) -> bool {
    is_unified(d.plan.as_ref())
}

/// The weekly included window, as (start, end) in epoch milliseconds.
///
/// The fingerprint `vendors::link_spacexsi` matches against Grok's window
/// to tell one account seen from both products from two accounts that
/// happen to share a machine. Milliseconds, because that is the precision
/// both vendors state it to, and the precision is what makes the match
/// worth trusting.
///
/// A response still held in the cache when its week ends is rolled onto
/// the week now running, as Grok's is, or for those minutes the same
/// account's two windows would not match.
pub fn pool_window(d: &Data) -> Option<(f64, f64)> {
    let live = d.live.as_ref()?;
    let (s, e) = match (loose(&live["billingCycleStart"]), loose(&live["billingCycleEnd"])) {
        (Some(s), Some(e)) if e > s => (s, e),
        _ => return None,
    };
    let at = now() * 1000.0;
    if e > at {
        return Some((s, e));
    }
    let weeks = ((at - e) / (e - s)).floor() + 1.0;
    Some((s + weeks * (e - s), e + weeks * (e - s)))
}

/// The monthly cycle additional usage resets on, as (start, end) epoch
/// milliseconds, each where stated.
///
/// GetMonthlyBillingCycle first, because it is the call that exists to say
/// this. GetPlanInfo's own `billingCycleEnd` is the fallback, and the two
/// disagreed by a day on the measured account - 12 Nov against 13 Nov - so
/// the fallback carries no start, and with no start there is no window to
/// pace a lane against.
fn month_of(d: &Data) -> (Option<f64>, Option<f64>) {
    if let Some(m) = d.month.as_ref() {
        let (s, e) = (loose(&m["startDateEpochMillis"]), loose(&m["endDateEpochMillis"]));
        if e.is_some() {
            return (s, e);
        }
    }
    let plan_end = d.plan.as_ref().and_then(|v| loose(&v["planInfo"]["billingCycleEnd"]));
    (None, plan_end)
}

/// Monthly additional usage against its limit, on the unified plan.
///
/// The limit is read exactly as on an account's own plan - the spend-limit
/// block and GetHardLimit did not change shape - but the spend comes from
/// the prepaid balance's `spentThisMonthCents`, which is the figure
/// cursor.com prints beside the limit. The spend-limit block omits its
/// spend at zero, so it agrees today, and the prepaid call is the one that
/// states it outright.
fn additional_state(d: &Data, live: &serde_json::Value) -> ExtraState {
    let mut state = extra_state(live, d.hard.as_ref());
    let spent = d.prepaid.as_ref().and_then(|v| loose(&v["spentThisMonthCents"]));
    // The account's own spend, so a user limit only: against a team pool
    // the pool's total is the figure, and one member's share of it would be
    // a partial presented as the whole.
    if let Some(spent) = spent {
        match &mut state {
            ExtraState::Fixed { used, shared: false, .. }
            | ExtraState::Unlimited { used }
            | ExtraState::Disabled { used } => *used = spent,
            ExtraState::Fixed { shared: true, .. } | ExtraState::NotReported { .. } => {}
        }
    }
    state
}

/// The prepaid balance as a lane: spent against paid, labelled with what
/// was paid. Nothing where the account is not on prepaid billing or has
/// paid nothing in - a percentage of nought is not a reading.
fn credits_lane(d: &Data) -> Option<Lane> {
    let prepaid = d.prepaid.as_ref()?;
    let paid = loose(&prepaid["fundedCents"]).filter(|v| *v > 0.0)?;
    // On a prepaid account an absent balance is proto3 omitting a zero.
    let left = loose(&prepaid["availableCents"]).unwrap_or(0.0);
    Some(Lane {
        label: PREPAID_LABEL.into(),
        pct: (100.0 * (paid - left) / paid).max(0.0),
        window_secs: None,
        reset: None,
        stale: false,
        projected: false,
        apart: false,
    })
}


/// Each product's colour in the pool bar and on its line under it.
///
/// Cursor and Grok Bot take their own agents' hues, so the split reads as
/// the same two products the rest of the widget colours that way; the
/// others are the existing palette's spare hues, each above AA on the
/// background and on the selected-row tint.
fn product_hue(name: &str) -> (u8, u8, u8) {
    match name {
        "Cursor" => agent_hue("cursor").unwrap(),
        "Grok Bot" => agent_hue("grok").unwrap(),
        "Grok Build" => (186, 166, 255),
        "Grok Chat" => (240, 200, 110),
        "Grok Imagine" => (232, 158, 200),
        _ => (206, 214, 228),
    }
}

/// The pool's percentage: the share of the weekly included allowance used.
///
/// `totalPercentUsed`, with the auto figure as its fallback. On the unified
/// plan the two are the same number - third-party models no longer draw on
/// the pool, so `apiPercentUsed` sits at nought and auto is the whole of it.
fn pool_pct(live: &serde_json::Value) -> Option<f64> {
    // A cached answer whose week has ended describes last week's pool: it
    // is not this week's figure, so it is withheld rather than drawn live.
    if loose(&live["billingCycleEnd"]).is_some_and(|e| e <= now() * 1000.0) {
        return None;
    }
    let plan = &live["planUsage"];
    loose(&plan["totalPercentUsed"]).or_else(|| loose(&plan["autoPercentUsed"]))
}

/// The SpaceXSI section of the Cursor tab: the weekly pool once, then the
/// two kinds of money beside it.
///
/// Replaces the QUOTA section only on the unified plan. One bar rather
/// than three: `cursor models` is the same number as the pool, and `other
/// models` is an included API allowance the plan no longer has - those
/// models bill to credits at list price - so it would sit at nought for
/// ever and read as an allowance nobody has touched. Grok Bot's allowance
/// is the same pool seen from Grok, and drawing it here as a second bar is
/// the triple count this section exists to end.
fn spacexsi_quota_with_plots(
    d: &Data,
    live: &serde_json::Value,
    w: usize,
    p: &Palette,
    plots: &mut Vec<tc::Plot>,
) -> Vec<String> {
    let (cycle_secs, reset_ts) = cycle_of(live);
    let when = match reset_ts.map(|e| e - now()) {
        Some(left) if left > 0.0 => format!(" · resets in {}", left_span(left)),
        Some(_) => " · resetting".to_string(),
        None => String::new(),
    };
    let via = if d.spacexsi == Spacexsi::Separate { "via Cursor · " } else { "" };
    // The countdown stands down whole when it does not fit: "resets " with
    // nothing after it is a sentence that stops.
    let head = format!(" ── {} ── ", SPACEXSI);
    let fixed = head.chars().count() + via.chars().count() + "live · weekly".chars().count();
    let when = if fixed + when.chars().count() <= w.saturating_sub(1) { when } else { String::new() };
    let mut rows = vec![tc::seg(
        &[
            (p.lbl.as_str(), head),
            (p.dim.as_str(), via.into()),
            (p.ok.as_str(), "live".into()),
            (p.dim.as_str(), format!(" · weekly{}", when)),
        ],
        w - 1,
    )];
    // Calendar time, as the QUOTA section's "of the cycle gone" is; the
    // pace cell beside the bar is the one that counts only work days.
    if let (Some(s), Some(e)) = (loose(&live["billingCycleStart"]), loose(&live["billingCycleEnd"])) {
        if e > s {
            let gone = (100.0 * (now() * 1000.0 - s) / (e - s)).clamp(0.0, 100.0);
            rows.push(tc::seg(
                &[(p.dim.as_str(), format!("  {:.0}% of the week gone", gone))],
                w - 1,
            ));
        }
    }
    let base = agent_hue("spacexsi").unwrap();
    let label = POOL_LABEL;
    let label_w = report_label_w();
    // The bar takes whatever the row has left once the label, the
    // percentage and the pace are placed, so it runs to the pane's edge.
    // At the narrowest the bar shrinks to a few cells rather than push the
    // percentage off the row; the pace stands down whole before that.
    let fill_w = |label_w: usize, pct: &str, pace: &str| {
        let fixed = 2 + label_w + 1 + pct.chars().count() + pace.chars().count();
        w.saturating_sub(1).saturating_sub(fixed).max(1)
    };
    let fits_pace = |label_w: usize, pct: &str, pace: &str| {
        2 + label_w + 1 + 3 + pct.chars().count() + pace.chars().count() <= w.saturating_sub(1)
    };
    let from_grok = pool_pct(live).is_none() && d.pool_from_grok.is_some();
    match pool_pct(live).or(d.pool_from_grok) {
        // Grok's split is there: the pool bar in one segment per product,
        // then each product on a line of its own in its segment's colour.
        Some(pct) if !d.pool_split.is_empty() => {
            let (pace_colour, pace_txt) =
                pace_cell(lead(pct, cycle_secs, reset_ts), pace_guessed(false, false, reset_ts), p);
            let pace_txt = if fits_pace(label_w, &pct_text(pct), &pace_txt) { pace_txt } else { String::new() };
            let bar_w = fill_w(label_w, &pct_text(pct), &pace_txt);
            // One colour per filled cell, each product's run as long as its
            // share of the bar; the track and the pace marker are the meter's
            // own, so the pool reads exactly as the plain bar does.
            let fill = ((pct / 100.0).clamp(0.0, 1.0) * bar_w as f64).round() as usize;
            let mut cells: Vec<String> = Vec::new();
            let mut upto = 0.0;
            for (name, share) in &d.pool_split {
                let (r, g, b) = product_hue(name);
                upto += share;
                let end = ((upto / 100.0) * bar_w as f64).round() as usize;
                while cells.len() < end.min(fill) {
                    cells.push(tc::rgb(r, g, b));
                }
            }
            // Cursor's figure is finer than Grok's whole percents, so a
            // sliver of the pool belongs to no product on the list. It is
            // drawn, dim, rather than handed to one of them.
            while cells.len() < fill {
                cells.push(p.dim.clone());
            }
            let mut line: Vec<(String, String)> =
                vec![(p.dim.clone(), format!("  {} ", tc::pad(label, label_w)))];
            let x = line.iter().map(|(_, t)| tc::display_width(t)).sum();
            let plot = tc::Plot::ramped_meter(
                x,
                rows.len(),
                bar_w,
                (pct / 100.0).clamp(0.0, 1.0),
                elapsed_of(cycle_secs, reset_ts),
                &cells,
                &p.grid,
                &p.pace_mark,
            );
            line.extend(plot.text_row(0).iter().cloned());
            plots.push(plot);
            line.push((pct_colour(pct, Some(base), p), pct_text(pct)));
            line.push((pace_colour, pace_txt));
            rows.push(seg_owned(&line, w));
            let name_w = d.pool_split.iter().map(|(n, _)| n.chars().count()).max().unwrap_or(0);
            // Under the bar where it fits; at a short indent where the bar's
            // column would push the share off the row.
            let need = 2 + name_w + 2 + "~100%".len();
            let indent = if label_w + 3 + need <= w.saturating_sub(1) { label_w + 3 } else { 4 };
            for (name, share) in &d.pool_split {
                let (r, g, b) = product_hue(name);
                let hue = tc::rgb(r, g, b);
                rows.push(seg_owned(
                    &[
                        (p.dim.clone(), " ".repeat(indent)),
                        (hue.clone(), "■ ".into()),
                        (hue, tc::pad(name, name_w)),
                        (p.txt.clone(), format!("  ~{:.0}%", share)),
                    ],
                    w,
                ));
            }
        }
        Some(pct) => {
            let (pace_colour, pace_txt) =
                pace_cell(lead(pct, cycle_secs, reset_ts), pace_guessed(false, false, reset_ts), p);
            let pace_txt = if fits_pace(label_w, &pct_text(pct), &pace_txt) { pace_txt } else { String::new() };
            let bar_w = fill_w(label_w, &pct_text(pct), &pace_txt);
            let mut line: Vec<(String, String)> =
                vec![(p.dim.clone(), format!("  {} ", tc::pad(label, label_w)))];
            line.extend(paced_bar_with_plot(plots, rows.len(), &line,
                (pct / 100.0).clamp(0.0, 1.0),
                elapsed_of(cycle_secs, reset_ts),
                bar_w,
                Some(base),
                p,
            ));
            line.push((pct_colour(pct, Some(base), p), pct_text(pct)));
            line.push((pace_colour, pace_txt));
            rows.push(seg_owned(&line, w));
        }
        None => rows.push(tc::seg(
            &[(p.dim.as_str(), "  no pool figure for this week".into())],
            w - 1,
        )),
    }
    // Said, since the heading reads `live` for Cursor's call: this one
    // figure is Grok's reading of the same pool. Dropped whole when it does
    // not fit rather than cut mid-sentence.
    if from_grok {
        let note = "  pool figure from Grok · Cursor sent none";
        if note.chars().count() <= w.saturating_sub(1) {
            rows.push(tc::seg(&[(p.dim.as_str(), note.into())], w - 1));
        }
    }
    rows.push(String::new());
    tc::append_plot_rows(&mut rows, plots, |local| extra_report(d, live, w, p, local));
    rows.push(String::new());
    tc::append_plot_rows(&mut rows, plots, |local| activity_rows(d, w, p, local));
    rows.push(String::new());
    rows
}

/// How many top-ups the report lists before it says how many more there are.
const TOPUPS_SHOWN: usize = 5;

/// Extra usage on the unified plan, as one report: the balance, this
/// month's spend against its limit, the rule that refills the balance, and
/// the refills themselves.
///
/// Everything past the weekly pool lands here - third-party models at list
/// price, and anything once the pool is spent - so this is where the money
/// is, and every figure on it is one Cursor states: GetUserPrepaidBilling
/// for the balance, the rule and the history, the spend-limit block and
/// GetHardLimit for the limit, GetMonthlyBillingCycle for the month. Two
/// things the same calls carry are left out on purpose: the min, max and
/// recommended limits are cursor.com's settings-page hints rather than
/// usage, and on the measured account the current limit sat below the
/// stated minimum, so drawing them would invite a reading nobody can trust.
fn extra_report(d: &Data, live: &serde_json::Value, w: usize, p: &Palette, plots: &mut Vec<tc::Plot>) -> Vec<String> {
    let (month_start, month_end) = month_of(d);
    let window = match (month_start, month_end) {
        (Some(s), Some(e)) => format!("month {} → {}", day_month(s), day_month(e)),
        (None, Some(e)) => format!("month ends {}", day_month(e)),
        _ => String::new(),
    };
    // The heading gives up its window whole: a month cut to "13 Oct → 1" is
    // a date that does not exist.
    let head = " ── EXTRA USAGE ── ";
    let room = w.saturating_sub(1).saturating_sub(head.chars().count());
    let window = if window.chars().count() <= room { window } else { String::new() };
    let mut rows = vec![tc::seg(&[(p.lbl.as_str(), head.into()), (p.dim.as_str(), window)], w - 1)];
    let label_w = report_label_w();
    // Each bar leads its block, and what it is a bar of - the dollars, and
    // the rule that refills them - sits under it, indented to the bar.
    let indent = " ".repeat(label_w + 3);
    let detail = |parts: Vec<(String, String)>, tails: Vec<String>| -> Vec<String> {
        // The figures are the reading, so they keep the row: where the
        // bar's column leaves too little room for them, they move left
        // to a short indent rather than be cut, and where even that is too
        // narrow they carry on to the next row a whole part at a time.
        let room_for = |lead: &str| w.saturating_sub(1).saturating_sub(lead.chars().count());
        let need: usize = parts.iter().map(|(_, t)| t.chars().count()).sum();
        let lead = if need <= room_for(&indent) { indent.clone() } else { "    ".to_string() };
        let mut lines: Vec<Vec<(String, String)>> = vec![Vec::new()];
        let mut used = 0;
        for (colour, part) in parts {
            let n = part.chars().count();
            if used > 0 && used + n > room_for(&lead) {
                lines.push(Vec::new());
                used = 0;
            }
            // A part opening its own row drops the space that joined it on.
            let part = if used == 0 { part.trim_start().to_string() } else { part };
            used += part.chars().count();
            lines.last_mut().unwrap().push((colour, part));
        }
        // The tails are the parts a narrow pane can do without, and each
        // goes whole or not at all, on the last row.
        let mut room = room_for(&lead).saturating_sub(used);
        for tail in tails {
            let n = tail.chars().count();
            if n <= room {
                room -= n;
                lines.last_mut().unwrap().push((p.dim.clone(), tail));
            }
        }
        lines
            .into_iter()
            .map(|parts| {
                let mut line: Vec<(String, String)> = vec![(p.dim.clone(), lead.clone())];
                line.extend(parts);
                seg_owned(&line, w)
            })
            .collect()
    };
    let prepaid = d.prepaid.as_ref();
    let left = prepaid.and_then(|v| loose(&v["availableCents"]));
    let on_prepaid = prepaid.is_some_and(|v| left.is_some() || text(v, "billingMode").contains("PREPAID"));
    let top = prepaid.map(|v| &v["autoTopUp"]);
    let topping = top.is_some_and(|t| t["enabled"].as_bool() == Some(true));

    // Prepaid credits: the bar, then the balance, the refill rule, and the
    // refills themselves - only for an account that has paid something in.
    // One that never has is not using them, and a block of zeros would read
    // as a balance it has run down.
    let credit_lane = if on_prepaid { credits_lane(d) } else { None };
    if let Some(lane) = credit_lane {
        rows.push(report_bar(&lane, PREPAID_LABEL, label_w, w, p, plots, rows.len()));
        let left = left.unwrap_or(0.0);
        let paid = prepaid.and_then(|v| loose(&v["fundedCents"])).filter(|v| *v > 0.0);
        let mut parts: Vec<(String, String)> = vec![
            (p.txt.clone(), money(left)),
            (p.dim.clone(), " left".into()),
        ];
        // Empty with nothing to refill it stops work, so it is never dropped.
        if left <= 0.0 && !topping {
            parts.push((p.warn.clone(), " · empty".into()));
        }
        // What was paid in, then what has been drawn - the subtraction of
        // two figures - are the parts a narrow pane stands down, whole.
        let mut tails: Vec<String> = Vec::new();
        if let Some(paid) = paid {
            tails.push(format!(" of {} paid", money(paid)));
            if paid >= left {
                tails.push(format!(" · {} used", money(paid - left)));
            }
        }
        rows.extend(detail(parts, tails));
        // The rule that refills the balance, and whether it can run.
        let mut parts: Vec<(String, String)> = vec![(p.dim.clone(), "auto top-up ".into())];
        let mut tails: Vec<String> = Vec::new();
        let mut below: Option<String> = None;
        match top {
            Some(t) if topping => {
                match (loose(&t["topUpAmountCents"]), loose(&t["thresholdCents"])) {
                    (Some(a), Some(b)) => {
                        parts.push((p.txt.clone(), money(a)));
                        parts.push((p.dim.clone(), " when below ".into()));
                        parts.push((p.txt.clone(), money(b)));
                    }
                    _ => parts.push((p.txt.clone(), "on".into())),
                }
                match prepaid.and_then(|v| v["hasPaymentMethod"].as_bool()) {
                    // Saved is reassurance and stands down whole; missing is
                    // the rule unable to run, and takes a line of its own
                    // rather than be dropped.
                    Some(true) => tails.push(" · payment method saved".into()),
                    Some(false) => below = Some("no payment method to charge".into()),
                    None => {}
                }
            }
            _ => parts.push((p.dim.clone(), "off".into())),
        }
        rows.extend(detail(parts, tails));
        if let Some(said) = below {
            let budget = w.saturating_sub(1).saturating_sub(indent.chars().count());
            if said.chars().count() <= budget {
                rows.push(tc::seg(&[(p.dim.as_str(), indent.clone()), (p.warn.as_str(), said)], w - 1));
            } else {
                for part in tc::wrap_words(&said, w.saturating_sub(3).max(1)) {
                    rows.push(tc::seg(&[(p.warn.as_str(), format!("  {part}"))], w - 1));
                }
            }
        }
        // The refills, newest first. A failed one is the rule firing and the
        // money not arriving, so it is said in the warning colour.
        if let Some(list) = prepaid.and_then(|v| v["topUps"].as_array()).filter(|l| !l.is_empty()) {
            let at = |v: &serde_json::Value| loose(&v["succeededAtMs"]).or_else(|| loose(&v["createdAtMs"]));
            let mut sorted: Vec<&serde_json::Value> = list.iter().collect();
            sorted.sort_by(|a, b| at(b).unwrap_or(0.0).total_cmp(&at(a).unwrap_or(0.0)));
            let amt_col = sorted
                .iter()
                .filter_map(|v| loose(&v["amountCents"]))
                .map(|c| money(c).chars().count())
                .max()
                .unwrap_or(0);
            let shown: Vec<&&serde_json::Value> = sorted.iter().take(TOPUPS_SHOWN).collect();
            for (i, t) in shown.iter().enumerate() {
                let Some(amount) = loose(&t["amountCents"]) else {
                    continue;
                };
                // A refill with no time on it is still a refill; it just has
                // no date to print, and a `?` would read as a corrupt one.
                let when = at(t).map(day_month).unwrap_or_default();
                let how = match text(t, "origin").as_str() {
                    o if o.contains("MANUAL") => "manual",
                    o if o.contains("AUTO") => "auto",
                    _ => "",
                };
                let status = text(t, "status");
                let failed = !status.is_empty() && !status.contains("SUCCEEDED");
                let mut parts: Vec<(String, String)> = vec![
                    (p.dim.clone(), if i == 0 { "top-ups ".into() } else { "        ".to_string() }),
                    (p.dim.clone(), format!("{:<6} ", when)),
                    (p.txt.clone(), format!("{:>1$}", money(amount), amt_col)),
                ];
                if !how.is_empty() {
                    parts.push((p.dim.clone(), format!(" {how}")));
                }
                if failed {
                    parts.push((p.warn.clone(), format!(" {}", status.to_lowercase().replace('_', " "))));
                }
                rows.extend(detail(parts, Vec::new()));
            }
            if sorted.len() > TOPUPS_SHOWN {
                rows.extend(detail(
                    vec![(p.dim.clone(), format!("        +{} earlier", sorted.len() - TOPUPS_SHOWN))],
                    Vec::new(),
                ));
            }
        }
    } else if prepaid.is_none() {
        // The call failed, which is not the same as nothing paid in.
        rows.push(tc::seg(
            &[
                (p.dim.as_str(), format!("  {} ", tc::pad(PREPAID_LABEL, label_w))),
                (p.warn.as_str(), "not reported".into()),
            ],
            w - 1,
        ));
    }

    // The monthly limit: a bar only where a limit is set, since a bar needs
    // a ceiling; then this month's spend against it, on the monthly clock.
    let state = additional_state(d, live);
    let when = month_end.map(|e| format!(" · resets {}", day_month(e)));
    match &state {
        ExtraState::Fixed { used, limit, shared } => {
            let secs = match (month_start, month_end) {
                (Some(s), Some(e)) if e > s => Some((e - s) / 1000.0),
                _ => None,
            };
            let lane = Lane {
                label: String::new(),
                pct: extra_lane_pct(&state).unwrap_or(0.0),
                window_secs: secs,
                reset: month_end.map(|e| e / 1000.0),
                stale: false,
                projected: false,
                apart: false,
            };
            rows.push(report_bar(&lane, LIMIT_LABEL, label_w, w, p, plots, rows.len()));
            let mut parts: Vec<(String, String)> = vec![
                (p.txt.clone(), money(*used)),
                (p.dim.clone(), " of ".into()),
                (p.txt.clone(), money(*limit)),
            ];
            // A team pool's scope is the fact the reader needs; "this month"
            // is the bar's own clock and stands down first.
            let mut tails: Vec<String> = Vec::new();
            if *shared {
                parts.push((p.dim.clone(), " team pool".into()));
            } else {
                tails.push(" this month".into());
            }
            // Over the limit states the overage, never a negative remainder,
            // and the overage is the one part of this line that is news.
            if used > limit {
                parts.push((p.bad.clone(), format!(" · {} over", money(used - limit))));
            } else {
                tails.push(format!(" · {} left", money(limit - used)));
            }
            tails.extend(when.clone());
            rows.extend(detail(parts, tails));
        }
        // No limit set and nothing spent: the account is not using it.
        ExtraState::Unlimited { used } | ExtraState::Disabled { used } if *used <= 0.0 => {}
        ExtraState::Unlimited { used } => {
            rows.push(tc::seg(&[(p.dim.as_str(), format!("  {} ", tc::pad(LIMIT_LABEL, label_w))), (p.dim.as_str(), "no limit set".into())], w - 1));
            rows.extend(detail(
                vec![(p.txt.clone(), money(*used)), (p.dim.clone(), " this month".into())],
                when.into_iter().collect(),
            ));
        }
        ExtraState::Disabled { used } => {
            rows.push(tc::seg(&[(p.dim.as_str(), format!("  {} ", tc::pad(LIMIT_LABEL, label_w))), (p.dim.as_str(), "disabled".into())], w - 1));
            if *used > 0.0 {
                rows.extend(detail(
                    vec![(p.txt.clone(), money(*used)), (p.dim.clone(), " this month, before it was disabled".into())],
                    Vec::new(),
                ));
            }
        }
        ExtraState::NotReported { keys } => {
            let mut line: Vec<(String, String)> = vec![
                (p.dim.clone(), format!("  {} ", tc::pad(LIMIT_LABEL, label_w))),
                (p.warn.clone(), "not reported".into()),
            ];
            if !keys.is_empty() {
                line.push((p.dim.clone(), format!(" · keys: {}", keys.join(", "))));
            }
            rows.push(seg_owned(&line, w));
        }
    }
    rows
}

/// The label column the SpaceXSI section and its extra-usage report share,
/// so every bar on the tab starts in one column.
fn report_label_w() -> usize {
    [POOL_LABEL, PREPAID_LABEL, LIMIT_LABEL].iter().map(|l| l.chars().count()).max().unwrap()
}

/// A bar under a figure in the extra-usage report, named the way `[+]`
/// names the same bar, so the tab and the summary share one vocabulary and
/// a bar can be read without the row above it. Then the percentage, and the
/// pace where the lane has a clock to pace against.
fn report_bar(lane: &Lane, label: &str, label_w: usize, w: usize, p: &Palette, plots: &mut Vec<tc::Plot>, row: usize) -> String {
    let hue = agent_hue("spacexsi").unwrap();
    // The same sizing as the pool row: the percentage is the reading and
    // keeps its cells; the pace stands down whole when it does not fit, and
    // the bar takes what is left, down to a single cell.
    let pct = pct_text(lane.pct);
    let lead_w = 2 + label_w + 1;
    let pace = lane
        .window_secs
        .map(|_| pace_cell(lead(lane.pct, lane.window_secs, lane.reset), pace_guessed(false, false, lane.reset), p))
        .filter(|(_, t)| lead_w + 3 + pct.chars().count() + t.chars().count() <= w.saturating_sub(1));
    let tail = pct.chars().count() + pace.as_ref().map(|(_, t)| t.chars().count()).unwrap_or(0);
    let bar_w = w.saturating_sub(1).saturating_sub(lead_w + tail).max(1);
    let mut line: Vec<(String, String)> = vec![(p.dim.clone(), format!("  {} ", tc::pad(label, label_w)))];
    line.extend(paced_bar_with_plot(plots, row, &line,
        (lane.pct / 100.0).clamp(0.0, 1.0),
        elapsed_of(lane.window_secs, lane.reset),
        bar_w,
        Some(hue),
        p,
    ));
    line.push((pct_colour(lane.pct, Some(hue), p), pct));
    if let Some(pace) = pace {
        line.push(pace);
    }
    seg_owned(&line, w)
}

/// A row from owned pairs.
fn seg_owned(parts: &[(String, String)], w: usize) -> String {
    let refs: Vec<(&str, String)> = parts.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
    tc::seg(&refs, w - 1)
}

/// The account's activity summary, from its own profile.
///
/// Two calls: the handle is not on disk, and the summary is keyed by it.
/// The same summary cursor.com/@handle draws its heatmap from, and on the
/// unified plan grok.com shows the same heatmap. The handle is the
/// account's public one, so asking by it reveals nothing the profile page
/// does not.
fn cursor_activity() -> Option<serde_json::Value> {
    let me = cursor_rpc("GetUserProfile", &serde_json::json!({}))?;
    let handle = text(&me["profile"], "handle");
    if handle.is_empty() {
        return None;
    }
    let got = cursor_rpc("GetViewableProfileByHandle", &serde_json::json!({ "handle": handle }))?;
    got.get("activitySummary").cloned()
}

/// The activity summary's daily counts, by date. A count is read as a
/// number or a string - Connect writes int64 as strings - and a day the
/// summary lists with no count is proto3 omitting a nought.
fn activity_days(summary: &serde_json::Value) -> HashMap<NaiveDate, f64> {
    summary["activityCounts"]
        .as_array()
        .map(|days| {
            days.iter()
                .filter_map(|day| {
                    let date = NaiveDate::parse_from_str(&text(day, "date"), "%Y-%m-%d").ok()?;
                    let n = loose(&day["count"]).or_else(|| loose(&day["value"])).unwrap_or(0.0);
                    Some((date, n))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The activity heatmap: tokens per day from the account's profile, its
/// peak, and its streaks.
///
/// Labelled as the profile's figures, because that is what they are: when
/// the plans merged, the day of the merge was still empty here while the
/// pool had moved, so whether this counts Grok's side of the plan as well
/// as Cursor's is not yet settled, and the heading does not claim it.
fn activity_rows(d: &Data, w: usize, p: &Palette, plots: &mut Vec<tc::Plot>) -> Vec<String> {
    let Some(summary) = d.activity.as_ref() else {
        return vec![tc::seg(
            &[(p.lbl.as_str(), " ── ACTIVITY ── ".into()), (p.warn.as_str(), "not reported".into())],
            w - 1,
        )];
    };
    let days = activity_days(summary);
    let Some(cal) = day_calendar(&days, w, SPACEXSI_STEPS, None, p) else {
        return vec![tc::seg(
            &[(p.lbl.as_str(), " ── ACTIVITY ── ".into()), (p.dim.as_str(), "no active days on the profile".into())],
            w - 1,
        )];
    };
    let peak = days.values().cloned().fold(0.0f64, f64::max);
    let best = cal
        .best
        .map(|b| format!(" on {} {}", MONTHS[b.month0() as usize], b.day()))
        .unwrap_or_default();
    // The heading gives up its parts whole, the peak's date first: "peak
    // 937.0M on" with no date after it is a sentence that stops.
    let mut head: Vec<(String, String)> = vec![
        (p.lbl.clone(), " ── ACTIVITY ── ".into()),
        (p.dim.clone(), "profile tokens / day · peak ".into()),
        (p.txt.clone(), big_num(peak)),
    ];
    let used: usize = head.iter().map(|(_, t)| t.chars().count()).sum();
    if used + best.chars().count() <= w.saturating_sub(1) {
        head.push((p.dim.clone(), best));
    }
    let mut rows = vec![seg_owned(&head, w)];
    // The streaks as the profile states them, where it does; the calendar's
    // own count otherwise.
    let current = loose(&summary["currentStreak"]).map(|v| v as usize).unwrap_or(cal.current);
    let longest = loose(&summary["longestStreak"]).map(|v| v as usize).unwrap_or(cal.longest);
    rows.push(tc::seg(
        &[(p.dim.as_str(), format!("  streak {} days · longest {}", current, longest))],
        w - 1,
    ));
    let mut plot = cal.plot.clone();
    plot.y += rows.len();
    plots.push(plot);
    for line in &cal.rows {
        rows.push(seg_owned(line, w));
    }
    let mut legend: Vec<(String, String)> = vec![(p.dim.clone(), "  Less ".into())];
    legend.extend(SPACEXSI_STEPS.iter().map(|(r, g, b)| (tc::rgb(*r, *g, *b), "█".to_string())));
    legend.push((p.dim.clone(), " More".into()));
    rows.push(seg_owned(&legend, w));
    rows
}

/// Per-model tokens and real cost over a window.
///
/// This is what the plan percentages are made of: which model spent the
/// money. totalCents is Cursor's own figure, not an estimate.
fn cursor_spend(days: i64) -> Option<serde_json::Value> {
    let at = (now() * 1000.0) as i64;
    cursor_rpc(
        "GetAggregatedUsageEvents",
        &serde_json::json!({
            "startDate": (at - days * 86_400_000).to_string(),
            "endDate": at.to_string(),
        }),
    )
}

/// The per-day running totals that pages of raw events fold into.
#[derive(Default)]
struct Tally {
    by_day_model: HashMap<String, HashMap<String, EventModel>>,
    charged_cents: f64,
    unmetered: u64,
    unpriced: u64,
    estimated: u64,
    tokens: f64,
    counted: u64,
}

#[derive(Default)]
struct EventModel {
    cents: f64,
    tokens: f64,
    priced: u64,
    unpriced: u64,
    invalid: bool,
}

fn nonnegative(v: &serde_json::Value) -> Option<f64> {
    loose(v).filter(|n| n.is_finite() && *n >= 0.0)
}

/// Cursor's input/cache counters are disjoint. Only omitted prices may
/// use a list-rate fallback; a reported zero is already a price.
fn event_price(usage: &serde_json::Value, model: &str) -> (Option<f64>, bool, bool) {
    if !usage["totalCents"].is_null() {
        let cents = nonnegative(&usage["totalCents"]);
        return (cents, false, cents.is_none());
    }
    let tokens: Tokens = [
        ("input", "inputTokens"),
        ("output", "outputTokens"),
        ("cache_read", "cacheReadTokens"),
        ("cache_write", "cacheWriteTokens"),
    ]
    .into_iter()
    .map(|(kind, field)| (kind.to_string(), nonnegative(&usage[field]).unwrap_or(0.0)))
    .collect();
    let model = cursor_price_model(model);
    let cents = rate_for(&model, &HashMap::new())
        .0
        .filter(|rate| rate_covers(&tokens, rate))
        .map(|rate| cost_of(&tokens, &rate) * 100.0)
        .filter(|n| n.is_finite());
    (cents, cents.is_some(), false)
}

fn cursor_price_model(model: &str) -> String {
    // claude-4.5-sonnet -> claude-sonnet-4-5, as in CodexBar's fallback.
    let parts: Vec<_> = model.split('-').collect();
    if parts.len() >= 3
        && parts[0] == "claude"
        && ["sonnet", "opus", "haiku"].contains(&parts[2])
        && parts[1].split('.').count() == 2
        && parts[1].chars().all(|c| c.is_ascii_digit() || c == '.')
    {
        let suffix = if parts.len() > 3 {
            format!("-{}", parts[3..].join("-"))
        } else {
            String::new()
        };
        format!(
            "claude-{}-{}{}",
            parts[2],
            parts[1].replace('.', "-"),
            suffix
        )
    } else {
        model.to_string()
    }
}

fn tally_events(events: &[serde_json::Value], start_ms: i64, end_ms: i64, t: &mut Tally) {
    for e in events {
        let Some(ms) = nonnegative(&e["timestamp"]).filter(|ms| *ms > 0.0) else {
            continue;
        };
        if ms < start_ms as f64 || ms > end_ms as f64 {
            continue;
        }
        let Some(day) = Local
            .timestamp_millis_opt(ms as i64)
            .single()
            .map(|d| d.format("%Y-%m-%d").to_string())
        else {
            continue;
        };
        t.counted += 1;
        match nonnegative(&e["chargedCents"]) {
            Some(cents) if (t.charged_cents + cents).is_finite() => t.charged_cents += cents,
            _ => t.unmetered += 1,
        }
        let usage = &e["tokenUsage"];
        let n = [
            "inputTokens",
            "outputTokens",
            "cacheReadTokens",
            "cacheWriteTokens",
        ]
        .iter()
        .map(|k| nonnegative(&usage[*k]).unwrap_or(0.0))
        .sum::<f64>();
        if n <= 0.0 || !n.is_finite() {
            continue;
        }
        let model = match text(e, "model") {
            s if s.is_empty() => "unknown".to_string(),
            s => s,
        };
        let (cents, estimated, invalid) = event_price(usage, &model);
        let slot = t.by_day_model.entry(day).or_default().entry(model).or_default();
        slot.tokens += n;
        slot.invalid |= invalid;
        if let Some(cents) = cents {
            slot.cents += cents;
            slot.invalid |= !slot.cents.is_finite();
            slot.priced += 1;
        } else {
            slot.unpriced += 1;
            t.unpriced += 1;
        }
        t.estimated += u64::from(estimated);
        t.tokens += n;
    }
}

fn event_window(days: i64, end_ms: i64) -> Option<i64> {
    let today = Local.timestamp_millis_opt(end_ms).single()?.date_naive();
    let first = today.checked_sub_signed(Days::days(days.checked_sub(1)?))?;
    // Use calendar days, not 24-hour subtraction across DST changes.
    Local
        .from_local_datetime(&first.and_hms_opt(0, 0, 0)?)
        .earliest()
        .map(|d| d.timestamp_millis())
}

fn collect_event_pages<F>(
    mut fetch: F,
    page_size: usize,
    max_pages: usize,
) -> Result<Vec<serde_json::Value>, String>
where
    F: FnMut(usize) -> Result<serde_json::Value, String>,
{
    let mut pages: Vec<Vec<serde_json::Value>> = Vec::new();
    let mut expected = None;
    let mut complete = false;
    for page in 1..=max_pages {
        let got = fetch(page)?;
        let obj = got.as_object().ok_or("invalid usage page")?;
        let count = match obj.get("totalUsageEventsCount") {
            None => None,
            Some(value) => Some(
                nonnegative(value)
                    .filter(|n| n.fract() == 0.0 && *n <= usize::MAX as f64)
                    .ok_or("invalid usage count")? as usize,
            ),
        };
        if let Some(count) = count {
            if expected.is_some_and(|old| old != count) {
                return Err("usage count changed while paging; refresh to retry".into());
            }
            expected = Some(count);
        }
        let events = match obj.get("usageEventsDisplay") {
            Some(v) => v.as_array().ok_or("invalid usage events")?.clone(),
            None if obj.is_empty() || (obj.len() == 1 && count.is_some()) => Vec::new(),
            None => return Err("usage response has no events".into()),
        };
        let short = events.len() < page_size;
        pages.push(events);
        if short {
            complete = true;
            break;
        }
    }
    if !complete {
        return Err("usage history reached the page limit; totals withheld".into());
    }
    let received: usize = pages.iter().map(Vec::len).sum();
    let expected = expected.unwrap_or(received);
    let mut excess = received
        .checked_sub(expected)
        .ok_or("incomplete usage history; totals withheld")?;
    let mut events = pages.first().cloned().unwrap_or_default();
    for pair in pages.windows(2) {
        let overlap = (1..=pair[0].len().min(pair[1].len()))
            .rev()
            .find(|n| pair[0][pair[0].len() - n..] == pair[1][..*n])
            .unwrap_or(0);
        let remove = overlap.min(excess);
        excess -= remove;
        events.extend_from_slice(&pair[1][remove..]);
    }
    if excess != 0 {
        return Err("inconsistent usage pages; totals withheld".into());
    }
    Ok(events)
}

fn event_report(
    events: &[serde_json::Value],
    days: i64,
    start_ms: i64,
    end_ms: i64,
) -> serde_json::Value {
    let mut t = Tally::default();
    tally_events(events, start_ms, end_ms, &mut t);
    let mut by_day_model = serde_json::Map::new();
    let mut by_day = serde_json::Map::new();
    for (day, models) in &t.by_day_model {
        let mut inner = serde_json::Map::new();
        let mut total = 0.0;
        for (model, value) in models {
            let cents = (!value.invalid && value.priced > 0).then_some(value.cents);
            total += cents.unwrap_or(0.0);
            inner.insert(
                model.clone(),
                serde_json::json!({ "cents": cents, "tokens": value.tokens,
                    "complete": !value.invalid && value.unpriced == 0 }),
            );
        }
        by_day.insert(day.clone(), serde_json::json!(total));
        by_day_model.insert(day.clone(), serde_json::Value::Object(inner));
    }
    serde_json::json!({
        "by_day": by_day,
        "by_day_model": by_day_model,
        "metered_cents": (t.counted > 0 && t.unmetered == 0).then_some(t.charged_cents),
        "unmetered": t.unmetered, "unpriced": t.unpriced, "estimated": t.estimated,
        "tokens": t.tokens,
        "events": t.counted,
        "days": days,
        "start_ms": start_ms, "end_ms": end_ms,
    })
}

fn cursor_events(days: i64) -> Result<serde_json::Value, String> {
    let end_ms = (now() * 1000.0) as i64;
    let start_ms = event_window(days, end_ms).ok_or("invalid usage window")?;
    let token = cursor_token().ok_or("no token - Cursor has not signed in here")?;
    let cookie = crate::parse::parse_cursor_cookie(&token, end_ms as f64 / 1000.0)
        .ok_or("Cursor token cannot open the dashboard; sign in again")?;
    let events = collect_event_pages(
        |page| {
            // Keep one identity and fixed bounds for the entire snapshot. Never
            // fall back to another endpoint or an unrelated browser session.
            post_json_said(
                "https://cursor.com/api/dashboard/get-filtered-usage-events",
                &[
                    ("Cookie", &cookie),
                    ("Origin", "https://cursor.com"),
                    ("Content-Type", "application/json"),
                    ("Accept", "application/json"),
                ],
                &serde_json::json!({ "page": page, "pageSize": EVENT_PAGE,
                "startDate": start_ms.to_string(), "endDate": end_ms.to_string() })
                .to_string(),
                30,
            )
            .map_err(|_| {
                "Cursor dashboard request failed; check login/network and refresh".to_string()
            })
        },
        EVENT_PAGE,
        EVENT_PAGES,
    )?;
    Ok(event_report(&events, days, start_ms, end_ms))
}

/// The authorship counts, straight out of the tracking database.
struct Tracking {
    hashes: i64,
    conversations: i64,
    models: i64,
    by_model: Vec<(String, i64)>,
    commits: i64,
    lines: i64,
    human_lines: i64,
    last: Option<f64>,
}

fn read_tracking(con: &Connection) -> rusqlite::Result<Tracking> {
    let (hashes, conversations, models) = con.query_row(
        "select count(*), count(distinct conversationId), count(distinct model) \
         from ai_code_hashes",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let mut stmt = con.prepare(
        "select model, count(*) from ai_code_hashes group by model order by 2 desc limit 8",
    )?;
    let by_model = stmt
        .query_map([], |r| {
            let name: Option<String> = r.get(0)?;
            Ok((
                match name {
                    Some(s) if !s.is_empty() => s,
                    _ => "?".to_string(),
                },
                r.get::<_, i64>(1)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let (commits, lines, human_lines) = con.query_row(
        "select count(*), sum(linesAdded), sum(humanLinesAdded) from scored_commits",
        [],
        |r| {
            Ok((
                r.get::<_, i64>(0)?,
                // sum() over no rows is NULL, and NULL here is a true zero -
                // a machine that has scored nothing - not a failure to read.
                r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                r.get::<_, Option<i64>>(2)?.unwrap_or(0),
            ))
        },
    )?;
    let last: Option<f64> =
        con.query_row("select max(timestamp) from ai_code_hashes", [], |r| r.get(0))?;
    Ok(Tracking {
        hashes,
        conversations,
        models,
        by_model,
        commits,
        lines,
        human_lines,
        last,
    })
}

pub fn read(caches: &mut Caches, _cfg: &Config) -> Data {
    let mut d = Data::default();
    // The published sections do not depend on the local database, so a
    // locked or missing file must not take the live quota down with it -
    // which is what usage.py's sqlite-error branch does.
    //
    // Every reading is keyed by account, the usage call's as much as the
    // plan's: the plan decides how the usage is drawn, so a usage response
    // held for the account signed in before would be drawn under the new
    // account's plan - a monthly figure labelled as a weekly pool.
    let me = account_key();
    let live_key = format!("cursor:{me}");
    let mut refuse = String::new();
    d.live = match cached(caches, &live_key, LIVE_TTL, || match cursor_live() {
        None => {
            refuse = "Cursor's usage endpoint did not answer".into();
            None
        }
        other => other,
    }) {
        Some(got) if !text(&got, "why").is_empty() => {
            d.live_why = text(&got, "why");
            None
        }
        Some(got) if got.get("u").is_some() => Some(got["u"].clone()),
        other => other,
    };
    if !refuse.is_empty() {
        d.live_why = refuse.clone();
        remember_refusal(caches, &live_key, &refuse);
    }
    // A plan cached for one account must never classify another.
    d.plan = cached(caches, &format!("cursor-plan:{me}"), PLAN_TTL, cursor_plan);
    // The plan decides which drawing the live figures get, so a failed
    // plan read keeps the last good one for this account rather than draw
    // a unified account's week as an old plan's three lanes.
    let last_plan = format!("cursor-plan-last:{me}");
    match d.plan.as_ref() {
        Some(plan) => {
            caches.live.insert(last_plan, (now(), Some(plan.clone()), f64::INFINITY));
        }
        None => {
            d.plan = caches.live.get(&last_plan).and_then(|(_, v, _)| v.clone());
        }
    }
    // Best-effort beside the usage call, and never able to take it down:
    // the cap in the usage block answers the state this account is in, and
    // this only tells an absent cap from a forbidden one.
    //
    // Held for as long as the usage call and no longer, because the two are
    // read together: the account can switch the limit off mid-cycle, and a
    // spend block refreshed every two minutes against an hour-old
    // `noUsageBasedAllowed` would draw the old state for the rest of that
    // hour. One small POST on the same schedule as the reading it qualifies.
    d.hard = cached(caches, &format!("cursor-hard:{me}"), LIVE_TTL, cursor_hard_limit);
    // The unified plan's money lives in two calls an account on its own
    // plan has no use for, so they are asked only once the plan says it is
    // unified. Best-effort like the hard limit: the pool bar stands without
    // either, and the rows they feed say nothing rather than a guess.
    if is_unified(d.plan.as_ref()) {
        d.prepaid = cached(caches, &format!("cursor-prepaid:{me}"), LIVE_TTL, || {
            cursor_rpc("GetUserPrepaidBilling", &serde_json::json!({}))
        });
        d.month = cached(caches, &format!("cursor-month:{me}"), PLAN_TTL, || {
            cursor_rpc("GetMonthlyBillingCycle", &serde_json::json!({}))
        });
        // Daily figures, so the plan's hour is plenty.
        d.activity = cached(caches, &format!("cursor-activity:{me}"), PLAN_TTL, cursor_activity);
    }
    let events_key = format!("cursor-events:{me}");
    let mut events_why = String::new();
    d.events = cached(caches, &events_key, EVENTS_TTL, || {
        match cursor_events(30) {
            Ok(events) => Some(events),
            Err(why) => {
                events_why = why;
                None
            }
        }
    });
    if !events_why.is_empty() {
        remember_refusal(caches, &events_key, &events_why);
        d.events = Some(serde_json::json!({"why": events_why}));
    }
    d.spend = cached(caches, &format!("cursor-spend:{me}"), LIVE_TTL, || {
        cursor_spend(30)
    });
    // Deliberately last of the four and deliberately unable to affect them:
    // this is an extra allowance, and an account without one is the normal
    // case rather than a fault.
    let mut why = String::new();
    d.sand = cached(caches, SAND_KEY, SAND_TTL, || match sand_status() {
        Ok(v) => Some(v),
        Err(e) => {
            why = e;
            None
        }
    });
    if d.sand.is_none() && !why.is_empty() {
        d.sand_why = why;
    }
    let path = under_home(".cursor/ai-tracking/ai-code-tracking.db");
    if !std::path::Path::new(&path).exists() {
        d.why = "no tracking database".into();
        d.db_missing = true;
        return d;
    }
    // Read-only is not decoration: this is a live agent's working state.
    let opened = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .and_then(|con| read_tracking(&con));
    match opened {
        Ok(t) => {
            d.ok = true;
            d.hashes = t.hashes;
            d.conversations = t.conversations;
            d.models = t.models;
            d.by_model = t.by_model;
            d.commits = t.commits;
            d.lines = t.lines;
            d.human_lines = t.human_lines;
            d.last = t.last;
        }
        // Locked or corrupt means the counts are unknown, never zero, and
        // the reason is kept for the tab to show.
        Err(e) => d.why = e.to_string().chars().take(40).collect(),
    }
    d
}

/// The billing cycle as (window seconds, reset epoch), where stated.
fn cycle_of(live: &serde_json::Value) -> (Option<f64>, Option<f64>) {
    match (loose(&live["billingCycleStart"]), loose(&live["billingCycleEnd"])) {
        (Some(s), Some(e)) if e > s => (Some((e - s) / 1000.0), Some(e / 1000.0)),
        _ => (None, None),
    }
}

/// Why Cursor publishes no bar on the summary, when it does not.
///
/// The tracking database is spend on this machine, not a live plan window,
/// so a tab full of authorship counts and an empty `[+]` row are not the
/// same fact.
pub fn why_no_lane(d: &Data) -> String {
    if !lanes(d).is_empty() {
        return String::new();
    }
    if !d.live_why.is_empty() {
        return format!("no quota · {}", d.live_why);
    }
    if d.live.is_some() {
        return "no quota · Cursor answered, and published no plan percentages for this period.".into();
    }
    "no quota · no live reading from Cursor's usage endpoint on this machine.".into()
}

/// Every quota this agent publishes, for the summary screen.
///
/// The same three percentages the tab draws, against the same billing
/// cycle. Never marked stale: the CLI keeps no quota cache on disk to fall
/// back to, so a reading is either recent or absent.
pub fn lanes(d: &Data) -> Vec<Lane> {
    let Some(live) = d.live.as_ref() else {
        return Vec::new();
    };
    let plan = &live["planUsage"];
    let (secs, reset) = cycle_of(live);
    let mut out = Vec::new();
    if unified(d) {
        // The pool once, on its weekly window - the usage call's cycle is
        // that window on this plan.
        if let Some(pct) = pool_pct(live) {
            out.push(Lane {
                label: POOL_LABEL.into(),
                pct,
                window_secs: secs,
                reset,
                stale: false,
                projected: false,
                apart: false,
            });
        }
        // Additional usage resets with the month, not the week, so it takes
        // the monthly cycle - the weekly reset beside a monthly limit is the
        // wrong day, which is what this lane drew before.
        let state = additional_state(d, live);
        if let Some(pct) = extra_lane_pct(&state) {
            let (start, end) = month_of(d);
            let month_secs = match (start, end) {
                (Some(s), Some(e)) if e > s => Some((e - s) / 1000.0),
                _ => None,
            };
            out.push(Lane {
                label: LIMIT_LABEL.into(),
                pct,
                window_secs: month_secs,
                reset: end.map(|e| e / 1000.0),
                stale: false,
                projected: false,
                apart: true,
            });
        }
        // What has been drawn from the prepaid balance, as a share of what
        // was paid into it. Money on the account rather than an allowance
        // on a clock, so it has no window: no reset to count down to and no
        // pace to judge, and the summary draws neither for it.
        if let Some(lane) = credits_lane(d) {
            out.push(lane);
        }
        return out;
    }
    for (label, key, _) in CURSOR_LANES {
        let Some(pct) = loose(&plan[*key]) else {
            continue;
        };
        out.push(Lane {
            label: (*label).to_string(),
            pct,
            window_secs: secs,
            reset,
            stale: false,
            projected: false,
                    apart: false,
        });
    }
    // Extra usage is monthly like the three above it and resets with
    // them, so it takes the billing cycle's window rather than one of its
    // own. A lane only where there is a ceiling to be a percentage of:
    // unlimited and disabled draw dollars on the tab and nothing here.
    //
    // The label carries the cap, because 19% of an unnamed limit is not
    // something a reader of the summary alone can act on - and over the
    // cap it is what makes a full red bar legible without the tab.
    let state = extra_state(live, d.hard.as_ref());
    if let Some(pct) = extra_lane_pct(&state) {
        let cap = match &state {
            ExtraState::Fixed { limit, .. } => cap_tag(limit / 100.0),
            _ => String::new(),
        };
        out.push(Lane {
            label: format!("extra{}", cap),
            pct,
            window_secs: secs,
            reset,
            stale: false,
            projected: false,
            // On the plan's cycle, but not one of the plan's three lanes:
            // those are a widening scope and this is on-demand money past
            // all of them. It shares their clock, which is why it is not
            // `grok bot` with a window of its own - and it is still not a
            // fourth slice of them, which is why it takes the break.
            apart: true,
        });
    }
    // Its own window, not the billing cycle's. The summary ranks lanes
    // against each other and prints each one's reset, so handing it the
    // monthly dates for a weekly allowance would misreport both.
    if let Some((pct, secs, reset)) = d.sand.as_ref().and_then(sand_lane) {
        out.push(Lane {
            label: "grok bot".to_string(),
            pct,
            window_secs: secs,
            reset,
            stale: false,
            projected: false,
            apart: true,
        });
    }
    out
}

/// The three lanes cursor-agent's Usage view shows, plus the cycle.
fn cursor_quota_with_plots(d: &Data, w: usize, p: &Palette, plots: &mut Vec<tc::Plot>) -> Vec<String> {
    let Some(live) = d.live.as_ref() else {
        return Vec::new();
    };
    let plan = &live["planUsage"];
    if plan.as_object().is_none_or(|o| o.is_empty()) {
        return Vec::new();
    }
    if unified(d) {
        return spacexsi_quota_with_plots(d, live, w, p, plots);
    }
    let when = match loose(&live["billingCycleEnd"]) {
        Some(ends) => {
            let left = ends / 1000.0 - now();
            if left > 0.0 {
                // Truncated days read "resets in 0d" for the whole of the
                // last day of a cycle, which is when it matters most.
                format!("resets in {}", left_span(left))
            } else {
                "resetting".to_string()
            }
        }
        None => String::new(),
    };
    let (cycle_secs, reset_ts) = cycle_of(live);
    let elapsed = match (loose(&live["billingCycleStart"]), loose(&live["billingCycleEnd"])) {
        (Some(s), Some(e)) if e > s => {
            Some((100.0 * (now() * 1000.0 - s) / (e - s)).clamp(0.0, 100.0))
        }
        _ => None,
    };
    let mut rows = vec![tc::seg(
        &[
            (p.lbl.as_str(), " ── QUOTA ── ".into()),
            (p.ok.as_str(), "live".into()),
            (
                p.dim.as_str(),
                crate::claude::scope_phrase(w, 17 + when.chars().count()).into(),
            ),
            (p.dim.as_str(), when.clone()),
        ],
        w - 1,
    )];
    if let Some(elapsed) = elapsed {
        // Just the fact; the +/- column it explains is on every tab now, so
        // a per-tab legend was both redundant and the thing that clipped.
        rows.push(tc::seg(
            &[(p.dim.as_str(), format!("  {:.0}% of the cycle gone", elapsed))],
            w - 1,
        ));
    }
    let base = agent_hue("cursor").unwrap();
    // Widest label decides the column, the way every other agent's lanes
    // already work - `agent-usage.rs` derives the same number from its own
    // pairs, which is why `premium reqs` and `gemini weekly` sit straight
    // while these three were squeezed into a hardcoded nine.
    //
    // Only the lanes that will actually draw. A lane whose field is absent
    // is skipped below, and counting it here would spend the column on a
    // row nobody sees - a response carrying only `total` would pad five
    // characters out to thirteen and take the other eight off the bar,
    // which is the same clipping this width was added to stop. Grok Bot
    // joins on the same terms: it is in the reckoning when its own row is
    // going to be drawn, and not otherwise.
    let label_w = CURSOR_LANES
        .iter()
        .filter(|(_, key, _)| loose(&plan[*key]).is_some())
        .map(|(name, _, _)| name.chars().count())
        .chain(
            d.sand
                .as_ref()
                .and_then(sand_lane)
                .map(|_| "grok bot".chars().count()),
        )
        .max()
        .unwrap_or(9);
    for (name, key, stop) in CURSOR_LANES {
        let Some(pct) = loose(&plan[*key]) else {
            continue;
        };
        let used = (pct / 100.0).clamp(0.0, 1.0);
        let hue = blend(base, *stop);
        // The +/- cell is how far ahead of the clock this lane is: the
        // share of the billing period gone minus the share of the
        // allowance spent. Positive is a cushion, negative means the lane
        // runs out before the cycle does. Pure arithmetic on the cycle
        // dates - nothing new is fetched for it.
        let (pace_colour, pace_txt) =
            pace_cell(lead(pct, cycle_secs, reset_ts), pace_guessed(false, false, reset_ts), p);
        let mut line: Vec<(String, String)> =
            vec![(p.dim.clone(), format!(" {}", tc::pad(name, label_w)))];
        line.extend(paced_bar_with_plot(plots, rows.len(), &line,
            used,
            elapsed_of(cycle_secs, reset_ts),
            w.saturating_sub(31 + label_w).max(8),
            Some(hue),
            p,
        ));
        line.push((pct_colour(pct, Some(hue), p), pct_text(pct)));
        line.push((pace_colour, pace_txt));
        let refs: Vec<(&str, String)> = line.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
        rows.push(tc::seg(&refs, w - 1));
    }
    // Grok Bot sits under the three plan lanes, after a gap, because it is
    // not one of them: a separate weekly allowance with its own reset,
    // which the heading above cannot speak for. So this row carries its
    // own countdown, and the blank line says it is not a fourth slice of
    // the thing above it.
    //
    // Full hue rather than a fourth step down the ramp, for two measured
    // reasons. The ramp encodes narrowing scope - total, then cursor
    // models, then other models - and this allowance is not narrower
    // than any of them, so a darker tint would state a relationship that
    // does not exist. And the ramp has no room left: the percentage is
    // drawn in the bar's own colour, other models at 0.62 already
    // measures 5.29 against the background, and the next step that reads
    // as distinct from it, 0.50, measures 4.10 - under AA. 0.56 clears at
    // 4.66 and is indistinguishable from other models by eye. Full hue
    // is 10.74, and the gap above does the work the colour would have been
    // doing badly.
    if let Some((pct, secs, reset)) = d.sand.as_ref().and_then(sand_lane) {
        let used = (pct / 100.0).clamp(0.0, 1.0);
        let hue = base;
        rows.push(String::new());
        let (pace_colour, pace_txt) =
            pace_cell(lead(pct, secs, reset), pace_guessed(false, false, reset), p);
        let resets = match reset.map(|e| e - now()) {
            Some(left) if left > 0.0 => format!("  {}", left_span(left)),
            Some(_) => "  resetting".to_string(),
            None => String::new(),
        };
        let mut line: Vec<(String, String)> =
            vec![(p.dim.clone(), format!(" {}", tc::pad("grok bot", label_w)))];
        line.extend(paced_bar_with_plot(plots, rows.len(), &line,
            used,
            elapsed_of(secs, reset),
            // Narrower than the plan lanes by the width of the reset cell,
            // so adding that column cannot push this row into a clip.
            w.saturating_sub(41 + label_w).max(8),
            Some(hue),
            p,
        ));
        line.push((pct_colour(pct, Some(hue), p), pct_text(pct)));
        line.push((pace_colour, pace_txt));
        line.push((p.dim.clone(), resets));
        let refs: Vec<(&str, String)> = line.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
        rows.push(tc::seg(&refs, w - 1));
    } else if !d.sand_why.is_empty() {
        rows.push(tc::seg(
            &[
                (p.dim.as_str(), " grok bot  ".into()),
                (p.warn.as_str(), d.sand_why.clone()),
            ],
            w - 1,
        ));
    }
    rows.extend(spend_rows(live, d.hard.as_ref(), w, p));
    rows.push(String::new());
    rows
}

/// One metered window over the per-day, per-model summary: dollars, tokens
/// and the models under them, costliest first.
fn window_of(by: &serde_json::Value, days: i64, today: NaiveDate) -> (f64, f64, Vec<Metered>) {
    let first = (today - Days::days(days - 1)).format("%Y-%m-%d").to_string();
    let last = today.format("%Y-%m-%d").to_string();
    let (mut cents, mut tokens) = (0.0, 0.0);
    // Per model, both figures the day already carries: Cursor's own events
    // state cents and tokens together, so its model rows can show their
    // tokens beside their dollars like every other metered tab.
    let mut models: HashMap<String, (Option<f64>, f64, bool)> = HashMap::new();
    for (day, entries) in by.as_object().into_iter().flatten() {
        if day.as_str() < first.as_str() || day.as_str() > last.as_str() {
            continue;
        }
        for (model, got) in entries.as_object().into_iter().flatten() {
            let cost = nonnegative(&got["cents"]);
            cents += cost.unwrap_or(0.0);
            tokens += num(got, "tokens");
            let seen = models.entry(model.clone()).or_insert((None, 0.0, true));
            if let Some(cost) = cost {
                seen.0 = Some(seen.0.unwrap_or(0.0) + cost);
            }
            seen.1 += num(got, "tokens");
            seen.2 &= cost.is_some() && got["complete"].as_bool().unwrap_or(true);
        }
    }
    let mut ranked: Vec<Metered> = models
        .into_iter()
        .map(|(m, (c, t, complete))| (m, c.map(|c| (c / 100.0, complete)), t))
        .collect();
    ranked.sort_by(|a, b| super::model_amount(&b.1).total_cmp(&super::model_amount(&a.1)));
    (cents / 100.0, tokens, ranked)
}

/// API-rate costs and charged cents from the same complete event snapshot.
fn cursor_metered(d: &Data, w: usize, p: &Palette) -> Vec<String> {
    let nothing = serde_json::Value::Null;
    let ev = d.events.as_ref().unwrap_or(&nothing);
    let why = text(ev, "why");
    if !why.is_empty() {
        return wrap_text(
            &format!("Cursor cost unavailable: {why}"),
            w.saturating_sub(4).max(20),
        )
        .into_iter()
        .map(|line| tc::seg(&[(p.warn.as_str(), format!("  {line}"))], w - 1))
        .collect();
    }
    let metered = nonnegative(&ev["metered_cents"]);
    let by = &ev["by_day_model"];
    if by.as_object().is_none_or(|o| o.is_empty())
        && metered.is_none()
        && ev["events"].as_u64().unwrap_or(0) == 0
    {
        return Vec::new();
    }
    let snapshot = ev["end_ms"]
        .as_i64()
        .and_then(|ms| Local.timestamp_millis_opt(ms).single());
    let today = snapshot
        .map(|at| at.date_naive())
        .unwrap_or_else(|| Local::now().date_naive());
    let span = match ev["days"].as_i64() {
        Some(days) if days > 0 => days,
        _ => 30,
    };
    let mut windows: Vec<Window> = Vec::new();
    for (label, days) in [("today", 1), ("30 days", span)] {
        let (cost, tokens, models) = window_of(by, days, today);
        windows.push((label.to_string(), cost, tokens, models));
    }
    let unpriced = ev["unpriced"].as_u64().unwrap_or(0);
    let estimated = ev["estimated"].as_u64().unwrap_or(0);
    let unmetered = ev["unmetered"].as_u64().unwrap_or(0);
    let mut caveat = format!("Cursor dashboard · {} requests · {} unpriced · {} estimated. Metered usage is not your bill.",
        ev["events"].as_u64().unwrap_or(0), unpriced, estimated);
    if let Some(at) = snapshot {
        caveat.push_str(&format!(" As of {}.", at.format("%Y-%m-%d %H:%M")));
    }
    if unmetered > 0 {
        caveat.push_str(&format!(
            " Metered total unavailable: {unmetered} events lack valid charged costs."
        ));
    }
    let mut rows = metered_block(
        "API-rate estimates",
        &windows,
        w,
        &[(
            "cursor meters".to_string(),
            metered.map(|cents| dollars(cents / 100.0, false)),
            p.txt.clone(),
        )],
        "",
        "account-wide",
        &caveat,
        p,
    );
    // Metered-only events have no token window for metered_block to draw.
    if rows.is_empty() {
        rows.push(tc::seg(
            &[(p.lbl.as_str(), " ── METERED ── account-wide".into())],
            w - 1,
        ));
        for line in wrap_text(&caveat, w.saturating_sub(4).max(20)) {
            rows.push(tc::seg(&[(p.dim.as_str(), format!("  {line}"))], w - 1));
        }
        if let Some(cents) = metered {
            rows.push(tc::seg(
                &[(p.txt.as_str(), format!(" cursor meters {}", money(cents)))],
                w - 1,
            ));
        }
    }
    rows
}

/// Spend per day, one column per day, the way the usage page shows it.
///
/// A bar chart rather than the calendar the token tabs use: this window is
/// thirty days, and thirty cells of a year-wide grid is six columns of
/// colour in a field of dots. Money over a month reads better as a
/// profile, and it is the shape Cursor's own dashboard draws.
fn cursor_daily(d: &Data, w: usize, p: &Palette) -> Vec<String> {
    let Some(ev) = d.events.as_ref() else {
        return Vec::new();
    };
    let Some(by_day) = ev["by_day"].as_object().filter(|o| !o.is_empty()) else {
        return Vec::new();
    };
    let days = match ev["days"].as_i64() {
        Some(n) if n > 0 => n,
        _ => 30,
    };
    let today = ev["end_ms"]
        .as_i64()
        .and_then(|ms| Local.timestamp_millis_opt(ms).single())
        .map(|at| at.date_naive())
        .unwrap_or_else(|| Local::now().date_naive());
    let series: Vec<NaiveDate> = (0..days).rev().map(|n| today - Days::days(n)).collect();
    let cents: Vec<f64> = series
        .iter()
        .map(|day| {
            by_day
                .get(&day.format("%Y-%m-%d").to_string())
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0)
        })
        .collect();
    let top = cents.iter().cloned().fold(0.0f64, f64::max);
    let peak = if top > 0.0 { top } else { 1.0 };
    // The peak's own day, or nothing when every day is zero - a peak of $0
    // has no date worth naming.
    let best = (top > 0.0)
        .then(|| cents.iter().position(|c| *c == top))
        .flatten()
        .map(|i| series[i]);
    let label = |d: &NaiveDate| format!("{} {}", MONTHS[d.month0() as usize], d.day());
    let mut rows = vec![tc::seg(
        &[
            (p.lbl.as_str(), " ── SPEND / DAY ── ".into()),
            (p.dim.as_str(), format!("{}d · peak ", days)),
            (p.agent.as_str(), format!("${:.0}", peak / 100.0)),
            (
                p.dim.as_str(),
                format!(
                    " on {}",
                    best.as_ref().map(&label).unwrap_or_else(|| "--".into())
                ),
            ),
            (p.dim.as_str(), " · today ".into()),
            (
                p.txt.as_str(),
                format!("${:.2}", cents.last().copied().unwrap_or(0.0) / 100.0),
            ),
        ],
        w - 1,
    )];
    let avail = w.saturating_sub(3).max(10);
    for line in wrap_text(
        &format!(
            "API-rate estimates · {} unpriced · {} estimated requests",
            ev["unpriced"].as_u64().unwrap_or(0),
            ev["estimated"].as_u64().unwrap_or(0)
        ),
        w.saturating_sub(4).max(20),
    ) {
        rows.push(tc::seg(&[(p.dim.as_str(), format!("  {line}"))], w - 1));
    }
    let mut cols: Vec<(f64, String)> = Vec::new();
    for (c, wide) in cents.iter().zip(tc::spread(cents.len(), avail)) {
        cols.extend(std::iter::repeat_n((*c, p.agent.clone()), wide));
    }
    for line in tc::vbars(&cols, 3, peak) {
        let mut parts: Vec<(&str, String)> = vec![(tc::RST, " ".into())];
        for (colour, ch) in &line {
            parts.push((colour.as_str(), ch.clone()));
        }
        rows.push(tc::seg(&parts, w - 1));
    }
    rows.push(tc::seg(
        &[(tc::RST, " ".into()), (p.grid.as_str(), "─".repeat(cols.len()))],
        w - 1,
    ));
    let (left, right) = (label(&series[0]), label(&series[series.len() - 1]));
    rows.push(tc::seg(
        &[
            (p.dim.as_str(), format!(" {}", left)),
            (
                p.dim.as_str(),
                " ".repeat(cols.len().saturating_sub(left.len() + right.len()).max(1)),
            ),
            (p.dim.as_str(), right),
        ],
        w - 1,
    ));
    rows.push(String::new());
    rows
}

/// The narrowest bar that still reads as one.
const MIN_BAR: usize = 6;

/// The row before the bar: two of indent, the name's twenty-six, ten of
/// right-aligned dollars, and the slack this bar has always been measured
/// from.
const SPEND_PREFIX: usize = 44;

/// Whether a spend row has the width for its token column beside the bar.
///
/// What is left after the columns before it has to hold the count, the two
/// spaces after it, and a bar worth drawing - or the count stands down
/// whole rather than being cut.
fn spend_tokens_fit(w: usize, tok_w: usize) -> bool {
    w.saturating_sub(SPEND_PREFIX) >= tok_w + 2 + MIN_BAR
}

/// How many cells the bar gets, once the token column has had its own.
///
/// The bar takes what is left rather than all of it: sized from the whole
/// width it would be clipped by `seg` exactly where the count sits, and a
/// bar cut at the right is a meter reading higher than it should.
fn spend_bar_width(w: usize, tok_w: usize, tokens: bool) -> usize {
    let taken = SPEND_PREFIX + if tokens { tok_w + 2 } else { 0 };
    w.saturating_sub(taken).max(MIN_BAR)
}

/// Where the money went, per model, over the last 30 days.
fn cursor_spend_with_plots(d: &Data, w: usize, p: &Palette, plots: &mut Vec<tc::Plot>) -> Vec<String> {
    let Some(spend) = d.spend.as_ref() else {
        return Vec::new();
    };
    let Some(aggs) = spend["aggregations"].as_array().filter(|a| !a.is_empty()) else {
        return Vec::new();
    };
    let grab = |key: &str| loose(&spend[key]).unwrap_or(0.0);
    let mut rows = vec![tc::seg(
        &[
            (p.lbl.as_str(), " ── SPEND ── ".into()),
            (p.dim.as_str(), "last 30d · ".into()),
            (p.agent.as_str(), format!("${:.2}", grab("totalCostCents") / 100.0)),
            (p.dim.as_str(), "  in ".into()),
            (p.txt.as_str(), big_num(grab("totalInputTokens"))),
            (p.dim.as_str(), " · out ".into()),
            (p.txt.as_str(), big_num(grab("totalOutputTokens"))),
            (p.dim.as_str(), " · cache ".into()),
            (p.txt.as_str(), big_num(grab("totalCacheReadTokens"))),
        ],
        w - 1,
    )];
    let mut models: Vec<&serde_json::Value> = aggs.iter().collect();
    let cents_of = |a: &serde_json::Value| loose(&a["totalCents"]).unwrap_or(0.0);
    models.sort_by(|a, b| cents_of(b).total_cmp(&cents_of(a)));
    let top = match cents_of(models[0]) {
        v if v != 0.0 => v,
        _ => 1.0,
    };
    // What each model's dollars bought, in the same `N tokens` shape the
    // shared METERED block uses, so a reader crossing from the CODEX tab
    // sees one format rather than two. The figures are the aggregation's
    // own, read through `loose` because Connect writes int64 as a string
    // and `as_f64` on one of those is a silent zero.
    let shown: Vec<&&serde_json::Value> = models.iter().take(6).collect();
    let tokens_of = |a: &serde_json::Value| {
        ["inputTokens", "outputTokens", "cacheReadTokens"]
            .iter()
            .map(|k| loose(&a[*k]).unwrap_or(0.0))
            .sum::<f64>()
    };
    let tokens_cell = |a: &serde_json::Value| format!("{} tokens", big_num(tokens_of(a)));
    let tok_w =
        shown.iter().map(|a| tokens_cell(a).chars().count()).max().unwrap_or(0);
    // The column and the bar share what is left rather than the bar taking
    // it: a bar that swallowed the space would push the count off a pane
    // with room for it, and a count cut to `1.` is a wrong number where a
    // missing column is only a narrower pane.
    let show_tokens = spend_tokens_fit(w, tok_w);
    let bar_w = spend_bar_width(w, tok_w, show_tokens);
    for a in shown {
        let cents = cents_of(a);
        let name = match text(a, "modelIntent") {
            s if s.is_empty() => "?".to_string(),
            s => s,
        };
        let mut parts = vec![
            (p.txt.clone(), format!("  {}", tc::pad(&name, 26))),
            (p.agent.clone(), format!("{:>9} ", format!("${:.2}", cents / 100.0))),
        ];
        if show_tokens {
            parts.push((p.dim.clone(), format!("{:>1$}  ", tokens_cell(a), tok_w)));
        }
        parts.extend(paced_bar_with_plot(plots, rows.len(), &parts, cents / top,
            None, bar_w, agent_hue("cursor"), p));
        let refs: Vec<_> = parts.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
        rows.push(tc::seg(&refs, w - 1));
    }
    rows.push(String::new());
    rows
}

fn cursor_plan_rows(d: &Data, w: usize, p: &Palette) -> Vec<String> {
    let Some(info) = d.plan.as_ref() else {
        return Vec::new();
    };
    let plan = &info["planInfo"];
    if plan.as_object().is_none_or(|o| o.is_empty()) {
        return Vec::new();
    }
    let mut pairs: Vec<(String, String)> = Vec::new();
    let price = text(plan, "price");
    if !price.is_empty() {
        pairs.push(("price".into(), price));
    }
    if let Some(inc) = loose(&plan["includedAmountCents"]).filter(|v| *v != 0.0) {
        pairs.push(("included".into(), format!("${:.2} per cycle", inc / 100.0)));
    }
    let owner = text(plan, "planOwner").replace("PLAN_OWNER_", "").to_lowercase();
    if !owner.is_empty() {
        pairs.push(("billing".into(), owner));
    }
    // Only the unified plan carries this, so an account on its own plan
    // draws the same pairs it always did.
    let period = text(plan, "includedUsagePeriod");
    if !period.is_empty() {
        let every = period.replace("INCLUDED_USAGE_PERIOD_", "").to_lowercase();
        pairs.push(("included".into(), format!("{} pool, shared with Grok", every)));
    }
    let note = if unified(d) { SPACEXSI } else { "" };
    plan_rows(&text(plan, "planName"), &pairs, w, note, None, "", p)
}

/// The tab's own body: the lanes, the spend charts, and the authorship
/// counts from the tracking database.
fn cursor_tab_with_plots(d: &Data, w: usize, p: &Palette, plots: &mut Vec<tc::Plot>) -> Vec<String> {
    cursor_body_with_plots(d, w, p, plots, true)
}

/// The tab's body, with or without the quota section at its head.
///
/// Without it is the merged SpaceXSI tab's half for Cursor: the pool, the
/// credits and the limit are drawn once above both products there, and
/// repeating them here would be the second copy that tab exists to avoid.
fn cursor_body_with_plots(d: &Data, w: usize, p: &Palette, plots: &mut Vec<tc::Plot>, quota: bool) -> Vec<String> {
    let mut rows = if quota { cursor_quota_with_plots(d, w, p, plots) } else { Vec::new() };
    if rows.is_empty() && quota {
        let note = why_no_lane(d);
        if !note.is_empty() {
            rows.extend(no_local(&note, "", w, p));
            rows.push(String::new());
        }
    }
    rows.extend(cursor_daily(d, w, p));
    tc::append_plot_rows(&mut rows, plots, |local| cursor_spend_with_plots(d, w, p, local));
    // SpaceXSI is drawn from the account's own figures only. The
    // authorship counts below are this machine's tracking database - one
    // machine's share of an account-wide plan - so the unified tab leaves
    // them out rather than set a local count beside cloud ones.
    if unified(d) {
        return rows;
    }
    if !d.ok {
        // An unreadable database makes the counts unknown, not zero, and
        // which kind of unreadable matters: absent means run the agent,
        // locked or corrupt names sqlite's own reason and will likely fix
        // itself. usage.py shows a fixed "no tracking database" either way,
        // which is false for a locked file.
        let why = if d.why.is_empty() { "not read yet" } else { d.why.as_str() };
        let note = no_local(
            &format!("No AI-written-code counts: {}.", why),
            if d.db_missing { run_hint("cursor") } else { "" },
            w,
            p,
        );
        if rows.is_empty() {
            return note;
        }
        return add_section(rows, note);
    }
    rows.push(tc::seg(
        &[
            (p.lbl.as_str(), " ── AI-WRITTEN CODE ── ".into()),
            (
                p.dim.as_str(),
                format!("last seen {} ago", ago(d.last.map(|ms| ms / 1000.0).unwrap_or(0.0))),
            ),
        ],
        w - 1,
    ));
    let (ai, human) = (d.lines, d.human_lines);
    let total = ai + human;
    let cells: Vec<(&str, String, &str)> = vec![
        ("tracked edits", commas(d.hashes), p.agent.as_str()),
        ("conversations", commas(d.conversations), p.txt.as_str()),
        ("scored commits", commas(d.commits), p.txt.as_str()),
        ("lines by agent", commas(ai), p.agent.as_str()),
        ("lines by hand", commas(human), p.txt.as_str()),
        ("models used", format!("{}", d.models), p.dim.as_str()),
    ];
    let label_w = cells.iter().map(|c| c.0.len()).max().unwrap_or(8);
    let ncols = if w.saturating_sub(2) / 2 >= label_w + 11 { 2 } else { 1 };
    let cw = w.saturating_sub(2) / ncols;
    let val_w = cw.saturating_sub(label_w + 3).max(5);
    for chunk in cells.chunks(ncols) {
        let mut line: Vec<(&str, String)> = vec![(tc::RST, " ".into())];
        for (label, value, colour) in chunk {
            line.push((p.dim.as_str(), format!(" {} ", tc::pad(label, label_w))));
            line.push((colour, tc::pad(value, val_w)));
        }
        rows.push(tc::seg(&line, w - 1));
    }
    if total > 0 {
        rows.push(String::new());
        rows.push(tc::seg(
            &[
                (p.lbl.as_str(), " ── WHO WROTE IT ── ".into()),
                (p.dim.as_str(), format!("{} lines scored", commas(total))),
            ],
            w - 1,
        ));
        let split = [
            (ai as f64 / total as f64, p.agent.clone()),
            (human as f64 / total as f64, p.dim.clone()),
        ];
        let mut bar: Vec<(&str, String)> = vec![(tc::RST, " ".into())];
        let plot = tc::Plot::gradient_stacked_bar(1, rows.len(), w.saturating_sub(3).max(10), &split);
        bar.extend(plot.text_row(0).iter().map(|(c, t)| (c.as_str(), t.clone())));
        rows.push(tc::seg(&bar, w - 1));
        plots.push(plot);
        rows.push(tc::seg(
            &[
                (
                    p.agent.as_str(),
                    format!(" ▇ agent {} ({:.0}%)", commas(ai), 100.0 * ai as f64 / total as f64),
                ),
                (
                    p.dim.as_str(),
                    format!(
                        "   ▇ hand {} ({:.0}%)",
                        commas(human),
                        100.0 * human as f64 / total as f64
                    ),
                ),
            ],
            w - 1,
        ));
    }
    if !d.by_model.is_empty() {
        rows.push(String::new());
        rows.push(tc::seg(
            &[
                (p.lbl.as_str(), " ── BY MODEL ── ".into()),
                (p.dim.as_str(), "tracked edits".into()),
            ],
            w - 1,
        ));
        let top = d.by_model[0].1.max(1);
        for (name, n) in d.by_model.iter().take(5) {
            let mut line = vec![
                (p.txt.clone(), format!("  {}", tc::pad(name, 22))),
                (p.agent.clone(), format!("{:>7} ", commas(*n))),
            ];
            line.extend(paced_bar_with_plot(plots, rows.len(), &line, *n as f64 / top as f64,
                None, w.saturating_sub(36).max(6), agent_hue("cursor"), p));
            let parts: Vec<_> = line.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
            rows.push(tc::seg(&parts, w - 1));
        }
    }
    rows.push(String::new());
    rows.push(tc::seg(
        &[(
            p.dim.as_str(),
            "  Authorship, not spend: this is how much code the agent wrote,".into(),
        )],
        w - 1,
    ));
    rows.push(tc::seg(
        &[(p.dim.as_str(), "  which is a different question from what it cost.".into())],
        w - 1,
    ));
    rows
}

/// The whole tab: the lanes and charts, what Cursor metered, and the plan
/// the percentages are percentages of.
pub fn tab_with_plots(d: &Data, w: usize, _h: usize, _cfg: &Config, p: &Palette, plots: &mut Vec<tc::Plot>) -> Vec<String> {
    // METERED sits last-but-one on every tab, so Cursor's - which is
    // published by the server rather than priced from a rate card - lands
    // in the same place as everyone else's rather than floating up beside
    // its quota.
    let body = add_section(cursor_tab_with_plots(d, w, p, plots), cursor_metered(d, w, p));
    add_section(body, cursor_plan_rows(d, w, p))
}

/// The SpaceXSI section on its own: the shared pool, the credits and the
/// limit, as the merged SpaceXSI tab draws them once above both products.
pub fn spacexsi_block_with_plots(d: &Data, w: usize, p: &Palette, plots: &mut Vec<tc::Plot>) -> Vec<String> {
    cursor_quota_with_plots(d, w, p, plots)
}

/// Everything on the tab except the quota section: the spend, the
/// authorship, what Cursor metered, and the plan.
pub fn details_with_plots(d: &Data, w: usize, p: &Palette, plots: &mut Vec<tc::Plot>) -> Vec<String> {
    let body = add_section(cursor_body_with_plots(d, w, p, plots, false), cursor_metered(d, w, p));
    add_section(body, cursor_plan_rows(d, w, p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spend_models_and_authorship_publish_gradient_plots_at_their_text_rows() {
        let d = Data {
            ok: true,
            lines: 30,
            human_lines: 70,
            by_model: vec![("model-a".into(), 100), ("model-b".into(), 40)],
            spend: Some(serde_json::json!({
                "totalCostCents": 14000,
                "aggregations": [
                    {"modelIntent": "model-a", "totalCents": 10000},
                    {"modelIntent": "model-b", "totalCents": 4000}
                ]
            })),
            ..Data::default()
        };
        for width in [60, 120] {
            let mut plots = Vec::new();
            let rows = cursor_tab_with_plots(&d, width, &palette(), &mut plots);
            let bars: Vec<_> = rows.iter().enumerate().filter(|(_, row)| {
                row.contains('█') || row.contains('░')
            }).collect();
            assert_eq!(bars.len(), 5, "two spend, two model and one authorship bar");
            assert_eq!(plots.len(), bars.len(), "a horizontal bar missed the pixel renderer");
            for (y, row) in bars {
                let plot = plots.iter().find(|plot| plot.y == y).expect("plot row drifted");
                let plain = strip(row);
                let at = plain.chars().position(|c| c == '█' || c == '░').unwrap();
                assert_eq!(plot.x, at, "plot covered a label: {plain}");
                let expected: String = plot.text_row(0).iter().map(|(_, t)| t.as_str()).collect();
                let shown: String = plain.chars().skip(at).take(plot.width).collect();
                assert_eq!(shown, expected);
                let colours: std::collections::HashSet<_> = plot.text_row(0).iter()
                    .filter(|(_, t)| t == "█").map(|(c, _)| c).collect();
                assert!(colours.len() > 1, "solid bar: {plain}");
            }
        }
    }

    fn empty_schema() -> Connection {
        let con = Connection::open_in_memory().unwrap();
        con.execute_batch(
            "create table ai_code_hashes \
               (hash text, conversationId text, model text, timestamp integer);\
             create table scored_commits \
               (hash text, linesAdded integer, humanLinesAdded integer);",
        )
        .unwrap();
        con
    }

    #[test]
    fn the_tracking_reader_counts_distinct_not_rows() {
        let con = empty_schema();
        // Two rows share a conversation so the distinct counts actually
        // discriminate from the plain ones.
        con.execute_batch(
            "insert into ai_code_hashes values
               ('a1', 'conv-1', 'model-a', 1000),
               ('a2', 'conv-1', 'model-a', 2000),
               ('a3', 'conv-2', 'model-b', 3000);\
             insert into scored_commits values ('c1', 120, 30), ('c2', 10, 5);",
        )
        .unwrap();
        let t = read_tracking(&con).unwrap();
        assert_eq!(t.hashes, 3);
        assert_eq!(t.conversations, 2);
        assert_eq!(t.models, 2);
        assert_eq!(t.by_model[0], ("model-a".to_string(), 2));
        assert_eq!(t.commits, 2);
        assert_eq!(t.lines, 130);
        assert_eq!(t.human_lines, 35);
        assert_eq!(t.last, Some(3000.0));
    }

    #[test]
    fn an_empty_database_reads_as_zeros_not_an_error() {
        // sum() over no rows is NULL, and NULL here is a true zero - a
        // machine that has scored nothing - not a failure to read.
        let t = read_tracking(&empty_schema()).unwrap();
        assert_eq!(t.commits, 0);
        assert_eq!(t.lines, 0);
        assert_eq!(t.human_lines, 0);
        assert_eq!(t.last, None);
    }

    #[test]
    fn a_broken_database_is_a_why_not_a_zero() {
        // No tables at all: the reader must error so the tab can say why,
        // never coast through with zeros that look like idleness.
        let con = Connection::open_in_memory().unwrap();
        let why = match read_tracking(&con) {
            Ok(_) => String::new(),
            Err(e) => e.to_string(),
        };
        assert!(!why.is_empty(), "a missing table must carry a reason");
    }

    /// A real-shaped response, from the field names CodexBar's Cursor
    /// provider decodes. Weekly window: 1700000000 -> 1700604800.
    fn sand(flag: serde_json::Value, pct: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "hasNonZeroIncludedLimit": flag,
            "usagePercent": pct,
            "currentPeriodStart": "2023-11-14T22:13:20Z",
            "nextResetTimestampUtc": "2023-11-21T22:13:20Z",
            "hasAvailableUsage": true,
        })
    }

    #[test]
    fn an_account_with_no_bot_allowance_draws_no_bot_bar() {
        // The whole point of the flag. A 0% bar here would state a limit
        // the account was never given, which is the one thing this widget
        // must not do - and 0% is exactly what an untouched allowance
        // looks like, so the two are indistinguishable without it.
        assert_eq!(sand_lane(&sand(serde_json::json!(false), serde_json::json!(0.0))), None);
        assert_eq!(sand_lane(&sand(serde_json::json!(null), serde_json::json!(12.0))), None);
        assert_eq!(sand_lane(&serde_json::json!({})), None);
        // The flag alone is not enough: no percentage, no bar.
        assert_eq!(sand_lane(&sand(serde_json::json!(true), serde_json::json!(null))), None);
    }

    #[test]
    fn the_bot_lane_keeps_its_own_week_not_the_billing_cycle() {
        let got = sand_lane(&sand(serde_json::json!(true), serde_json::json!(62.5)));
        let (pct, secs, reset) = got.expect("an allowance the account has");
        assert!((pct - 62.5).abs() < 1e-9);
        // Seven days, not the month the plan lanes run to.
        assert_eq!(secs, Some(604_800.0));
        assert_eq!(reset, Some(1_700_604_800.0));

        // And it reaches the summary screen as its own lane, alongside the
        // plan's - which is what puts it on [+].
        let d = Data {
            live: Some(serde_json::json!({
                "planUsage": { "totalPercentUsed": 9.5 },
                "billingCycleStart": "1700000000000",
                "billingCycleEnd": "1702592000000",
            })),
            sand: Some(sand(serde_json::json!(true), serde_json::json!(62.5))),
            ..Data::default()
        };
        let lanes = lanes(&d);
        assert_eq!(lanes.len(), 2);
        let bot = lanes.iter().find(|l| l.label == "grok bot").expect("a grok bot lane");
        assert_eq!(bot.window_secs, Some(604_800.0), "took the monthly window");
        assert_eq!(bot.reset, Some(1_700_604_800.0), "took the monthly reset");
        // The plan lane beside it must be untouched by any of this.
        let tot = lanes.iter().find(|l| l.label == "total").unwrap();
        assert_eq!(tot.window_secs, Some(2_592_000.0));
    }

    #[test]
    fn a_bot_reading_that_failed_leaves_the_plan_bars_alone() {
        // Best-effort by contract: this lane is an extra, and an extra that
        // can take the tab down with it is worse than no extra.
        let d = Data {
            live: Some(serde_json::json!({
                "planUsage": {
                    "totalPercentUsed": 9.5, "limit": 40000.0,
                    // The honest pair, and beside it the one the line used
                    // to read: totalSpend carries bonusSpend, so it is not
                    // spend against the limit and must not be drawn as it.
                    "includedSpend": 33099.0, "totalSpend": 172364.0,
                },
                "billingCycleStart": "1700000000000",
                "billingCycleEnd": "1702592000000",
            })),
            sand: None,
            sand_why: "cursor.com did not answer".into(),
            ..Data::default()
        };
        assert_eq!(lanes(&d).len(), 1, "a failed extra invented or removed a lane");
        let rows = cursor_quota(&d, 110, &palette());
        let joined = rows.join("\n");
        assert!(joined.contains("total"), "plan bar lost: {}", joined);
        assert!(joined.contains("$330.99"), "spend lost: {}", joined);
        assert!(!joined.contains("$1723.64"), "bonus spend drawn as plan spend: {}", joined);
        assert!(joined.contains("did not answer"), "reason not shown: {}", joined);
    }

    #[test]
    fn a_lane_that_publishes_nothing_is_absent_not_zero() {
        // autoPercentUsed is missing, so no "cursor models" lane may appear
        // - a fabricated 0% is indistinguishable from an untouched lane.
        let d = Data {
            live: Some(serde_json::json!({
                "planUsage": { "totalPercentUsed": 41.5, "apiPercentUsed": "2.5" },
                "billingCycleStart": "1700000000000",
                "billingCycleEnd": "1702592000000",
            })),
            ..Data::default()
        };
        let got = lanes(&d);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].label, "total");
        assert!((got[0].pct - 41.5).abs() < 1e-9);
        // Connect writes int64 as strings; the cycle still has to be read.
        assert_eq!(got[0].window_secs, Some(2_592_000.0));
        assert_eq!(got[0].reset, Some(1_702_592_000.0));
        // A percentage that arrives as a string is still a percentage.
        assert_eq!(got[1].label, "other models");
        assert!((got[1].pct - 2.5).abs() < 1e-9);
        assert!(lanes(&Data::default()).is_empty());
    }

    /// The bar and the token column share what the row leaves them.
    ///
    /// A bar sized from the whole width would be clipped by `seg` exactly
    /// where the count is drawn, which reads as a fuller meter than the
    /// figure it stands for.
    #[test]
    fn the_bar_leaves_the_token_column_its_cells() {
        // From the first width with room for a bar at all: below that the
        // floor wins and `seg` clips, which is this block's behaviour from
        // before there was a column to share with.
        for w in SPEND_PREFIX + MIN_BAR..=200 {
            for tok_w in [9, 11, 13, 14] {
                let tokens = spend_tokens_fit(w, tok_w);
                let column = if tokens { tok_w + 2 } else { 0 };
                assert!(
                    spend_bar_width(w, tok_w, tokens) + column
                        <= w.saturating_sub(SPEND_PREFIX),
                    "{} wide, a {}-cell count",
                    w,
                    tok_w
                );
            }
        }
    }

    /// Every spend row says what its dollars bought, and the count stands
    /// down whole rather than being cut when the pane cannot hold it beside
    /// a bar worth drawing.
    #[test]
    fn a_spend_row_carries_its_tokens_and_drops_them_before_cutting_them() {
        // Every token field here is a JSON string, which is how Connect
        // writes int64 and how the live call answers.
        let d = Data {
            spend: Some(serde_json::json!({
                "totalCostCents": "125413",
                "totalInputTokens": "1300000000",
                "totalOutputTokens": "61200000",
                "totalCacheReadTokens": "39000000",
                "aggregations": [
                    { "modelIntent": "claude-opus-5", "totalCents": "89176",
                      "inputTokens": "1300000000", "outputTokens": "61200000",
                      "cacheReadTokens": "39000000" },
                    { "modelIntent": "composer-1", "totalCents": "4",
                      "inputTokens": "400000", "outputTokens": "100000",
                      "cacheReadTokens": "0" },
                ],
            })),
            ..Data::default()
        };
        let p = palette();
        let whole = ["1.4B tokens", "500.0k tokens"];
        let (mut with, mut without) = (false, false);
        for w in 20..=160 {
            let rows: Vec<String> =
                cursor_spend_rows(&d, w, &p).iter().map(|r| strip(r)).collect();
            let model_rows: Vec<&String> =
                rows.iter().filter(|r| r.contains("claude-opus-5") || r.contains("composer-1")).collect();
            assert_eq!(model_rows.len(), 2, "{} wide: {:#?}", w, rows);
            for row in &model_rows {
                assert!(row.chars().count() <= w - 1, "{} wide: {:?}", w, row);
                // The bar never goes away for the column beside it. Below
                // about forty-five columns the name and the dollars alone
                // fill the row and `seg` has always clipped the bar off -
                // that is this block's oldest behaviour, not this column's.
                if w >= 45 {
                    assert!(row.contains('█') || row.contains('░'), "{} wide: {:?}", w, row);
                }
                if row.contains("token") {
                    assert!(whole.iter().any(|t| row.contains(t)), "{} wide: {:?}", w, row);
                }
            }
            let here = model_rows.iter().any(|r| r.contains("token"));
            with |= here;
            without |= !here;
        }
        assert!(with && without);
        // Wide enough: the count is there, and it is the aggregation's own
        // figure rather than the zero a plain as_f64 would read off a
        // string.
        let wide: Vec<String> = cursor_spend_rows(&d, 160, &p).iter().map(|r| strip(r)).collect();
        assert!(
            wide.iter().any(|r| r.contains("claude-opus-5") && r.contains("1.4B tokens")),
            "{:#?}",
            wide
        );
        // Narrow enough that the bar would have nothing left: no count, and
        // no clipped half of one.
        let narrow: Vec<String> = cursor_spend_rows(&d, 60, &p).iter().map(|r| strip(r)).collect();
        assert!(!narrow.iter().any(|r| r.contains("token")), "{:#?}", narrow);
    }

    #[test]
    fn a_window_takes_only_its_own_days() {
        let today = NaiveDate::parse_from_str("2026-08-23", "%Y-%m-%d").unwrap();
        let by = serde_json::json!({
            "2026-08-23": { "model-x": { "cents": 250.0, "tokens": 1000.0 } },
            "2026-08-01": { "model-x": { "cents": 100.0, "tokens": 400.0 },
                            "model-y": { "cents": 500.0, "tokens": 900.0 } },
            "2026-07-01": { "model-x": { "cents": 9999.0, "tokens": 9.0 } },
        });
        let (cost, tokens, _) = window_of(&by, 1, today);
        assert!((cost - 2.50).abs() < 1e-9);
        assert!((tokens - 1000.0).abs() < 1e-9);
        // Thirty days reaches 1 August but not 1 July.
        let (cost, _, models) = window_of(&by, 30, today);
        assert!((cost - 8.50).abs() < 1e-9);
        // Costliest model first, in dollars, with the tokens that cost it.
        assert_eq!(models[0].0, "model-y");
        let (cost, complete) = models[0].1.expect("a vendor rate priced it");
        assert!((cost - 5.0).abs() < 1e-9);
        assert!(complete, "Cursor's events carry their own rate");
        assert!((models[0].2 - 900.0).abs() < 1e-9);
    }

    #[test]
    fn events_use_identical_bounds_and_include_cache_writes() {
        let cut = now() - 30.0 * 86400.0;
        let recent = ((now() - 3600.0) * 1000.0) as i64;
        let ancient = ((cut - 86400.0) * 1000.0) as i64;
        let events = vec![
            serde_json::json!({
                "timestamp": recent.to_string(), "model": "model-x",
                "tokenUsage": { "totalCents": "12.5",
                                "inputTokens": "100", "outputTokens": "50",
                                "cacheReadTokens": "25", "cacheWriteTokens": "10" },
                "chargedCents": "8",
            }),
            serde_json::json!({
                "timestamp": ancient.to_string(), "model": "model-x",
                "tokenUsage": { "totalCents": 999.0,
                                "inputTokens": 9, "outputTokens": 9 },
            }),
        ];
        let mut t = Tally::default();
        tally_events(
            &events,
            (cut * 1000.0) as i64,
            (now() * 1000.0) as i64,
            &mut t,
        );
        assert_eq!(t.counted, 1);
        assert!((t.charged_cents - 8.0).abs() < 1e-9);
        assert!((t.tokens - 185.0).abs() < 1e-9);
    }

    #[test]
    fn the_quota_names_all_three_lanes_and_the_spend_in_dollars() {
        let p = palette();
        let start = ((now() - 10.0 * 86400.0) * 1000.0) as i64;
        let end = ((now() + 20.0 * 86400.0) * 1000.0) as i64;
        let d = Data {
            live: Some(serde_json::json!({
                "planUsage": {
                    "totalPercentUsed": 41.0, "autoPercentUsed": 12.0,
                    "apiPercentUsed": 3.0,
                    // Strings, as Connect writes int64 - and includedSpend
                    // rather than totalSpend, which is inflated by the
                    // bonus Cursor granted past the included amount.
                    "limit": "40000", "includedSpend": "16400",
                    "totalSpend": "99999", "remaining": "23600",
                },
                "billingCycleStart": start.to_string(),
                "billingCycleEnd": end.to_string(),
            })),
            ..Data::default()
        };
        let joined = cursor_quota(&d, 100, &p).join("\n");
        for want in [
            "total", "cursor models", "other models",
            "$164.00", "$400.00", "$236.00", "included",
            "of the cycle gone", "resets in",
        ] {
            assert!(joined.contains(want), "missing {}", want);
        }
        assert!(!joined.contains("$999.99"), "totalSpend drawn against the limit: {}", joined);
    }

    #[test]
    fn an_omitted_included_spend_is_nought_against_the_limit() {
        // Proto3 omits a field sitting at its default. A cycle that has
        // not spent against the included amount yet still has a limit,
        // and zip-ing the pair used to drop the whole row for that.
        let mut v = measured();
        v["planUsage"].as_object_mut().unwrap().remove("includedSpend");
        let joined = spend_rows(&v, None, 100, &palette())
            .iter()
            .map(|r| strip(r))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("$0.00 of $400.00 included"), "{}", joined);
        assert!(!joined.contains("$1792.80"), "bonus spend filled the gap: {}", joined);
    }

    /// A drawn row with runs of spaces folded to one, so a check reads the
    /// words and not the width of the label column.
    fn squeeze(text: &str) -> String {
        text.split(' ').filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ")
    }

    /// A drawn row without its colour escapes, so a column index means
    /// cells on screen rather than bytes in the string.
    fn strip(row: &str) -> String {
        let mut out = String::new();
        let mut chars = row.chars();
        while let Some(c) = chars.next() {
            if c != '\x1b' {
                out.push(c);
                continue;
            }
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        }
        out
    }

    #[test]
    fn every_lane_starts_its_bar_in_the_same_column() {
        // The labels are not the same length, so the column has to be as
        // wide as the longest of them or the bars step raggedly to the
        // right. It was a hardcoded nine, which fitted the old three-letter
        // names and nothing since - `agent-usage.rs` has always derived
        // this from its own pairs, which is why `premium reqs` and
        // `gemini weekly` sit straight while these did not.
        let p = palette();
        let start = (now() - 5.0 * 86400.0) * 1000.0;
        let end = (now() + 25.0 * 86400.0) * 1000.0;
        let d = Data {
            live: Some(serde_json::json!({
                "planUsage": {
                    "totalPercentUsed": 41.0,
                    "autoPercentUsed": 12.0,
                    "apiPercentUsed": 59.0,
                },
                "billingCycleStart": start.to_string(),
                "billingCycleEnd": end.to_string(),
            })),
            ..Data::default()
        };
        // Narrow is where a mis-set width shows: the bar budget is taken
        // from what the label left behind, so an over-long label eats the
        // bar rather than the margin.
        for width in [60usize, 80, 120] {
            let rows = cursor_quota(&d, width, &p);
            let bars: Vec<usize> = rows
                .iter()
                .filter_map(|row| {
                    let plain = strip(row);
                    CURSOR_LANES
                        .iter()
                        .any(|(name, _, _)| plain.contains(name))
                        .then(|| plain.find('█').or_else(|| plain.find('░')))
                        .flatten()
                })
                .collect();
            assert_eq!(bars.len(), CURSOR_LANES.len(), "a lane went missing at {}", width);
            assert!(
                bars.windows(2).all(|w| w[0] == w[1]),
                "bars start in different columns at width {}: {:?}",
                width,
                bars
            );
        }
    }

    #[test]
    fn an_absent_lane_does_not_spend_the_column_it_would_have_used() {
        // A response carrying only `total` is a supported shape - the lane
        // beside this one proves it - and the column has to be five wide,
        // not the thirteen `cursor models` would have wanted. Those eight
        // cells come straight off the bar, which is the clipping the
        // derived width exists to prevent rather than cause.
        let p = palette();
        let start = (now() - 5.0 * 86400.0) * 1000.0;
        let end = (now() + 25.0 * 86400.0) * 1000.0;
        let only_total = Data {
            live: Some(serde_json::json!({
                "planUsage": { "totalPercentUsed": 41.0 },
                "billingCycleStart": start.to_string(),
                "billingCycleEnd": end.to_string(),
            })),
            ..Data::default()
        };
        let all_three = Data {
            live: Some(serde_json::json!({
                "planUsage": {
                    "totalPercentUsed": 41.0,
                    "autoPercentUsed": 12.0,
                    "apiPercentUsed": 59.0,
                },
                "billingCycleStart": start.to_string(),
                "billingCycleEnd": end.to_string(),
            })),
            ..Data::default()
        };
        let bar_start = |d: &Data| -> usize {
            let rows = cursor_quota(d, 80, &p);
            let row = rows
                .iter()
                .map(|r| strip(r))
                .find(|r| r.contains("total"))
                .expect("a total lane");
            row.find('█').or_else(|| row.find('░')).expect("a bar")
        };
        let lone = bar_start(&only_total);
        let crowded = bar_start(&all_three);
        assert!(
            lone < crowded,
            "the lone lane paid for labels that never drew: {} vs {}",
            lone,
            crowded
        );
    }

    #[test]
    fn an_unreadable_database_says_why_instead_of_zeros() {
        let p = palette();
        let d = Data {
            why: "database is locked".into(),
            ..Data::default()
        };
        let joined = cursor_tab(&d, 90, &p).join("\n");
        assert!(joined.contains("database is locked"));
        assert!(!joined.contains("AI-WRITTEN CODE"));
    }

    #[test]
    fn the_metered_section_uses_event_charges_not_aggregate_or_savings() {
        let p = palette();
        let today = Local::now().date_naive().format("%Y-%m-%d").to_string();
        let d = Data {
            events: Some(serde_json::json!({
                "by_day": { today.clone(): 1000.0 },
                "by_day_model": {
                    today: { "model-x": { "cents": 1000.0, "tokens": 5000.0 } },
                },
                "metered_cents": 400.0,
                "days": 30,
            })),
            // Connect writes the metered figure as a string too.
            spend: Some(serde_json::json!({ "totalCostCents": "999999" })),
            ..Data::default()
        };
        let joined = cursor_metered(&d, 100, &p).join("\n");
        for want in [
            "account-wide",
            "$10.00",
            "cursor meters",
            "$4.00",
            "not your bill",
        ] {
            assert!(joined.contains(want), "missing {}", want);
        }
        assert!(!joined.contains("the plan saves"));
        assert!(!joined.contains("9999.99"));
    }

    #[test]
    fn dashboard_pagination_covers_43000_events_and_rejects_partial_histories() {
        let mut calls = 0;
        let all = collect_event_pages(|page| {
            calls += 1;
            let rows: Vec<_> = if page <= 43 { (0..1000).map(|i|
                serde_json::json!({"timestamp": page * 1000 + i})).collect() } else { Vec::new() };
            Ok(serde_json::json!({"totalUsageEventsCount": "43000", "usageEventsDisplay": rows}))
        }, 1000, 200).unwrap();
        assert_eq!(calls, 44);
        assert_eq!(all.len(), 43000);
        assert!(collect_event_pages(
            |_| Ok(serde_json::json!({"usageEventsDisplay": [1, 2]})),
            2,
            8
        )
        .is_err());
        assert!(collect_event_pages(
            |p| if p == 1 {
                Ok(serde_json::json!({"usageEventsDisplay": [1, 2], "totalUsageEventsCount": 3}))
            } else {
                Err("offline".into())
            },
            2,
            8
        )
        .is_err());
        assert!(collect_event_pages(
            |_| Ok(serde_json::json!({"usageEventsDisplay": [1], "totalUsageEventsCount": 3})),
            2,
            8
        )
        .is_err());
        assert!(
            collect_event_pages(|_| Ok(serde_json::json!({"error": "unauthorized"})), 2, 8)
                .is_err()
        );
        assert!(collect_event_pages(|_| Ok(serde_json::json!({})), 2, 8)
            .unwrap()
            .is_empty());
        assert_eq!(
            collect_event_pages(
                |page| Ok(if page == 1 {
                    serde_json::json!({"usageEventsDisplay": [1, 2], "totalUsageEventsCount": 2})
                } else {
                    serde_json::json!({})
                }),
                2,
                8
            )
            .unwrap()
            .len(),
            2
        );
    }

    #[test]
    fn only_count_proven_boundary_duplicates_are_removed() {
        for (total, want) in [(3, vec![1, 2, 3]), (4, vec![1, 2, 2, 3])] {
            let all = collect_event_pages(|page| Ok(serde_json::json!({
                "totalUsageEventsCount": total,
                "usageEventsDisplay": match page { 1 => vec![1, 2], 2 => vec![2, 3], _ => vec![] }
            })), 2, 5).unwrap();
            assert_eq!(
                all,
                want.into_iter()
                    .map(serde_json::Value::from)
                    .collect::<Vec<_>>()
            );
        }
        assert!(collect_event_pages(
            |page| Ok(serde_json::json!({
                "totalUsageEventsCount": page,
                "usageEventsDisplay": [1, 2]
            })),
            2,
            5
        )
        .is_err());
    }

    #[test]
    fn reported_zero_is_not_estimated_and_unknown_or_invalid_cost_is_not_free() {
        let model = "claude-sonnet-4-5";
        let usage = serde_json::json!({"inputTokens": 1000000, "cacheWriteTokens": 1000000});
        let (estimate, estimated, invalid) = event_price(&usage, model);
        let rate = rate_for(model, &HashMap::new()).0.unwrap();
        assert_eq!(
            estimate,
            Some((rate["input"] + rate["cache_write"]) * 100.0)
        );
        assert!(estimated && !invalid);
        assert_eq!(cursor_price_model("claude-4.5-sonnet"), model);
        assert_eq!(
            event_price(&usage, "unknown-future-model"),
            (None, false, false)
        );
        let mut reported = usage;
        reported["totalCents"] = serde_json::json!(0);
        assert_eq!(event_price(&reported, model), (Some(0.0), false, false));
        for bad in [
            serde_json::json!(-1),
            serde_json::json!("NaN"),
            serde_json::json!("bad"),
        ] {
            reported["totalCents"] = bad;
            assert_eq!(event_price(&reported, model), (None, false, true));
        }
    }

    #[test]
    fn missing_charges_withhold_metered_total_but_keep_token_coverage() {
        let end = Local::now().timestamp_millis();
        let start = event_window(30, end).unwrap();
        let rows = vec![
            serde_json::json!({"timestamp": end, "model": "unknown-model", "chargedCents": 4,
                "tokenUsage": {"inputTokens": 10}}),
            serde_json::json!({"timestamp": end, "model": "priced-model",
                "tokenUsage": {"totalCents": 10, "inputTokens": 20}}),
            serde_json::json!({"timestamp": end, "chargedCents": 2}),
            serde_json::json!({"timestamp": start - 1, "chargedCents": 900}),
            serde_json::json!({"timestamp": end + 1, "chargedCents": 900}),
        ];
        let report = event_report(&rows, 30, start, end);
        assert!(report["metered_cents"].is_null());
        assert_eq!(report["unmetered"], 1);
        assert_eq!(report["unpriced"], 1);
        assert_eq!(report["events"], 3);
        assert_eq!(report["tokens"], 30.0);
        let shown = cursor_metered(
            &Data {
                events: Some(report),
                ..Data::default()
            },
            100,
            &palette(),
        )
        .join("\n");
        assert!(shown.contains("at least $0.10"));
        assert!(shown.contains("1 unpriced"));
        assert!(shown.contains("Metered total unavailable"));
        assert!(!shown.contains("cursor meters"));
    }

    #[test]
    fn event_window_starts_at_local_midnight_29_days_ago() {
        let end = Local
            .with_ymd_and_hms(2026, 10, 14, 16, 0, 0)
            .single()
            .unwrap();
        let start = Local
            .timestamp_millis_opt(event_window(30, end.timestamp_millis()).unwrap())
            .single()
            .unwrap();
        assert_eq!(
            start.format("%Y-%m-%d %H:%M").to_string(),
            "2026-09-15 00:00"
        );
    }

    #[test]
    fn thousands_read_as_thousands() {
        assert_eq!(commas(0), "0");
        assert_eq!(commas(999), "999");
        assert_eq!(commas(1_000), "1,000");
        assert_eq!(commas(1_482_113), "1,482,113");
    }

    #[test]
    fn a_missing_quota_says_which_step_failed() {
        let token = Data {
            live_why: "no token - Cursor has not signed in here".into(),
            ..Data::default()
        };
        let note = why_no_lane(&token);
        assert!(note.starts_with("no quota · "), "{note}");
        assert!(note.contains("not signed in"), "{note}");

        let empty = why_no_lane(&Data::default());
        assert!(empty.contains("no live reading"), "{empty}");
        assert!(lanes(&Data::default()).is_empty());

        let refused = Data {
            live_why: "Cursor's usage endpoint did not answer".into(),
            ..Data::default()
        };
        let note = why_no_lane(&refused);
        assert!(note.contains("did not answer"), "{note}");
        assert!(lanes(&refused).is_empty());
    }
    /// The response measured on this account, trimmed to the block under
    /// test. Every number is cents, as the service sends them.
    fn measured() -> serde_json::Value {
        serde_json::json!({
            "billingCycleStart": "1786518073000",
            "billingCycleEnd": "1789196473000",
            "planUsage": {
                "totalSpend": 179265, "includedSpend": 40000, "bonusSpend": 139265,
                "limit": 40000, "remainingBonus": false,
                "autoPercentUsed": 43.0, "apiPercentUsed": 100, "totalPercentUsed": 51.2
            },
            "spendLimitUsage": {
                "totalSpend": 964, "individualLimit": 5000,
                "individualUsed": 964, "individualRemaining": 4036,
                "limitType": "user"
            },
            "enabled": true,
        })
    }

    /// Just the extra-usage row, without its colour escapes - the row the
    /// state table is about.
    fn extra_line(live: &serde_json::Value, hard: Option<&serde_json::Value>, w: usize) -> String {
        strip(spend_rows(live, hard, w, &palette()).last().expect("an extra row"))
    }

    /// GetHardLimit as it answered here: dollars, and nothing else set,
    /// which is proto3 leaving every other field at its default.
    fn hard_fifty() -> serde_json::Value {
        serde_json::json!({ "hardLimit": 50 })
    }

    #[test]
    fn the_measured_account_is_a_fixed_fifty_dollar_cap() {
        assert_eq!(
            extra_state(&measured(), Some(&hard_fifty())),
            ExtraState::Fixed { used: 964.0, limit: 5000.0, shared: false }
        );
        // 9.64 of 50 is 19.28%, and the lane says so rather than rounding
        // to a number that could be anything.
        let pct = extra_lane_pct(&extra_state(&measured(), Some(&hard_fifty()))).unwrap();
        assert!((pct - 19.28).abs() < 1e-9, "{pct}");
        // And the same state without the second call, because the cap is in
        // the usage block and the hard limit is only ever a second opinion.
        assert_eq!(
            extra_state(&measured(), None),
            ExtraState::Fixed { used: 964.0, limit: 5000.0, shared: false }
        );
    }

    #[test]
    fn the_cap_is_read_whether_it_arrives_as_a_number_or_a_string() {
        // Connect writes int64 as JSON strings, and which fields come
        // through as strings is not something this end controls. A cap read
        // with a plain as_f64 would be silently absent, and an absent cap
        // is a different state from a cap of $50.
        let mut v = measured();
        v["spendLimitUsage"] = serde_json::json!({
            "totalSpend": "964", "individualLimit": "5000",
            "individualUsed": "964", "individualRemaining": "4036",
            "limitType": "user"
        });
        assert_eq!(
            extra_state(&v, None),
            ExtraState::Fixed { used: 964.0, limit: 5000.0, shared: false }
        );
    }

    #[test]
    fn a_limit_type_that_is_not_user_is_a_shared_pool() {
        // CodexBar's mapping: a team account without a personal cap draws
        // on the team's on-demand budget instead, which is a different
        // population and has to be labelled as one.
        let mut v = measured();
        v["spendLimitUsage"]["limitType"] = serde_json::json!("team");
        assert_eq!(
            extra_state(&v, None),
            ExtraState::Fixed { used: 964.0, limit: 5000.0, shared: true }
        );
        assert!(extra_line(&v, None, 100).contains("of $50.00 team pool"), "pool not labelled");

        // The pool's spend, not this account's. On the measured user account
        // the two figures were equal; a team with other spenders is the
        // case the label is for, and individualUsed against a team ceiling
        // is a partial presented as a total.
        v["spendLimitUsage"]["individualUsed"] = serde_json::json!(964);
        v["spendLimitUsage"]["totalSpend"] = serde_json::json!(4000);
        assert_eq!(
            extra_state(&v, None),
            ExtraState::Fixed { used: 4000.0, limit: 5000.0, shared: true }
        );
        let line = extra_line(&v, None, 100);
        assert!(line.contains("$40.00 of $50.00 team pool"), "{}", line);
        assert!(!line.contains("$9.64"), "this account's spend drawn as the pool: {}", line);
        assert_eq!(extra_lane_pct(&extra_state(&v, None)), Some(80.0));

        // An absent limitType is proto3's empty string and says nothing
        // about a pool, so it must not be read as one.
        let mut bare = measured();
        bare["spendLimitUsage"] = serde_json::json!({ "individualUsed": 964, "individualLimit": 5000 });
        assert_eq!(
            extra_state(&bare, None),
            ExtraState::Fixed { used: 964.0, limit: 5000.0, shared: false }
        );
    }

    #[test]
    fn a_cap_lowered_under_what_is_spent_states_the_overage() {
        // The account can set the cap to $1 after spending $10, and then
        // both figures are real. Never a negative remainder, never a
        // percentage clamped in the text, and a lane the summary can draw
        // full - the bar clamps, the number does not.
        let mut v = measured();
        v["spendLimitUsage"] = serde_json::json!({
            "totalSpend": 1000, "individualLimit": 100,
            "individualUsed": 1000, "individualRemaining": -900,
            "limitType": "user"
        });
        let state = extra_state(&v, None);
        assert_eq!(state, ExtraState::Fixed { used: 1000.0, limit: 100.0, shared: false });
        assert_eq!(extra_lane_pct(&state), Some(1000.0));
        let line = extra_line(&v, None, 100);
        assert!(line.contains("$10.00 of $1.00 limit"), "{}", line);
        assert!(line.contains("$9.00 over"), "overage not stated: {}", line);
        assert!(!line.contains("left"), "a negative remainder was drawn: {}", line);
        assert!(!line.contains("-$"), "a negative figure was drawn: {}", line);
    }

    #[test]
    fn a_cap_raised_mid_cycle_is_a_percentage_of_the_new_one() {
        // Nothing is cached against the old cap: the percentage is computed
        // from whatever the response says on the read that follows the
        // change, which is the only way a mid-cycle change can be honest.
        let mut v = measured();
        let pct = |v: &serde_json::Value| extra_lane_pct(&extra_state(v, None)).unwrap();
        assert!((pct(&v) - 19.28).abs() < 1e-9, "{}", pct(&v));
        v["spendLimitUsage"]["individualLimit"] = serde_json::json!(10000);
        assert!((pct(&v) - 9.64).abs() < 1e-9, "{}", pct(&v));
    }

    #[test]
    fn an_unlimited_cap_draws_dollars_and_no_lane() {
        // ASSUMED WIRE SHAPE, not measured. Only the fixed state has been
        // seen on a real account. Proto3 omits a field at its default, so
        // unlimited is taken to arrive as the block with a spend in it and
        // no individualLimit, with nothing in GetHardLimit forbidding extra
        // usage. Replace this fixture with a captured response when the
        // account owner toggles the setting.
        let mut v = measured();
        v["spendLimitUsage"] = serde_json::json!({
            "totalSpend": 964, "individualUsed": 964, "limitType": "user"
        });
        let state = extra_state(&v, Some(&serde_json::json!({})));
        assert_eq!(state, ExtraState::Unlimited { used: 964.0 });
        // A bar needs a ceiling. This one has none, so there is no lane -
        // the refusal sand_lane makes for an allowance nobody granted.
        assert_eq!(extra_lane_pct(&state), None);
        let line = extra_line(&v, None, 100);
        assert!(line.contains("$9.64 · no limit"), "{}", line);
        assert!(!line.contains(" of "), "a denominator was invented: {}", line);
    }

    #[test]
    fn extra_usage_switched_off_keeps_what_was_already_spent() {
        // ASSUMED WIRE SHAPE, not measured. noUsageBasedAllowed is the
        // field cursor.com's dashboard carries beside hardLimit, and the
        // only one that can say extra usage is forbidden rather than
        // merely uncapped; the RPC answered hardLimit alone here, which is
        // consistent with it sitting at its false default.
        let off = serde_json::json!({ "hardLimit": 0, "noUsageBasedAllowed": true });
        // Something spent before it was switched off is billable money and
        // stays on screen. It is not a percentage of anything any more, so
        // the lane goes.
        let mut spent = measured();
        spent["spendLimitUsage"] = serde_json::json!({
            "totalSpend": 1000, "individualUsed": 1000, "limitType": "user"
        });
        let state = extra_state(&spent, Some(&off));
        assert_eq!(state, ExtraState::Disabled { used: 1000.0 });
        assert_eq!(extra_lane_pct(&state), None);
        // Without the flag the same block is an uncapped spend, which is
        // the point of the flag: it is the only thing that says forbidden.
        assert_eq!(extra_state(&spent, None), ExtraState::Unlimited { used: 1000.0 });
        let line = extra_line(&spent, Some(&off), 100);
        assert!(line.contains("$10.00 · disabled"), "{}", line);

        // Nothing spent: the state is the whole of it, and no figure is
        // drawn because $0.00 of $0.00 would be a cap of nothing.
        let mut clean = measured();
        clean["spendLimitUsage"] = serde_json::json!({ "individualUsed": 0, "limitType": "user" });
        assert_eq!(extra_state(&clean, Some(&off)), ExtraState::Disabled { used: 0.0 });
        let line = extra_line(&clean, Some(&off), 100);
        assert!(line.contains("extra usage  disabled"), "{:?}", line);
        assert!(!line.contains("$"), "a zero cap was drawn: {:?}", line);
    }

    #[test]
    fn a_block_that_is_absent_or_unrecognised_says_so_and_guesses_nothing() {
        // Four states, and the fourth is the one that matters most: a shape
        // this parser has not seen must never be drawn as one of the three
        // it has. An absent block is not a cap of zero, and not "no limit"
        // either - it is a thing not reported.
        let mut v = measured();
        v.as_object_mut().unwrap().remove("spendLimitUsage");
        assert_eq!(extra_state(&v, Some(&hard_fifty())), ExtraState::NotReported { keys: Vec::new() });
        let line = extra_line(&v, Some(&hard_fifty()), 100);
        assert!(line.contains("extra usage  not reported"), "{:?}", line);
        assert!(!line.contains("$"), "the hard limit was drawn as a cap: {:?}", line);
        assert_eq!(extra_lane_pct(&extra_state(&v, None)), None);

        // A block carrying neither a spend nor a cap: the keys go on the
        // line, so an unmapped shape can be read off the pane.
        let mut odd = measured();
        odd["spendLimitUsage"] = serde_json::json!({ "somethingNew": 12, "limitType": "user" });
        match extra_state(&odd, None) {
            ExtraState::NotReported { keys } => {
                assert!(keys.contains(&"somethingNew".to_string()), "{:?}", keys)
            }
            other => panic!("guessed a state from a shape it has not seen: {:?}", other),
        }
        let line = extra_line(&odd, None, 100);
        assert!(line.contains("somethingNew"), "keys not named: {:?}", line);

        // A zero cap is no cap. It must not draw as $0.00 of $0.00.
        let mut zero = measured();
        zero["spendLimitUsage"] = serde_json::json!({ "individualLimit": 0, "limitType": "user" });
        assert_eq!(extra_state(&zero, None), ExtraState::NotReported {
            keys: vec!["individualLimit".into(), "limitType".into()]
        });
        let line = extra_line(&zero, None, 100);
        assert!(!line.contains("of $0.00"), "a cap of nothing was drawn: {:?}", line);
    }

    #[test]
    fn the_extra_lane_joins_the_plan_lanes_on_the_summary() {
        // On [+] it is ranked with everything else, on the plan's own
        // cycle, because extra usage resets when the cycle does.
        let d = Data {
            live: Some(measured()),
            hard: Some(hard_fifty()),
            ..Data::default()
        };
        let got = lanes(&d);
        let extra = got.iter().find(|l| l.label.starts_with("extra")).expect("an extra lane");
        assert!((extra.pct - 19.28).abs() < 1e-9, "{}", extra.pct);
        assert_eq!(extra.window_secs, Some(2_678_400.0));
        assert_eq!(extra.reset, Some(1_789_196_473.0));
        // It shares the plan's clock and is still set apart from the plan's
        // three lanes: those are a widening scope, this is on-demand money
        // past all of them, and a reader running down four bars in a row
        // takes the fourth for another slice of the plan.
        assert!(extra.apart, "extra usage reads as a fourth slice of the plan");
        assert_eq!(extra.label, "extra $50", "the lane does not name its ceiling");
        // And it is an addition, not a replacement: the three plan lanes
        // are still there beside it.
        assert_eq!(got.len(), 4);
    }

    #[test]
    fn both_dollar_lines_line_up_and_neither_wraps() {
        // The two sit together and are read as a pair, so the amounts share
        // a column. And the pane this runs in is 58 cells wide: a row
        // wider than its pane is worse than one that was cut, so the date
        // stands down before the money does.
        let p = palette();
        let d = Data {
            live: Some(measured()),
            hard: Some(hard_fifty()),
            ..Data::default()
        };
        for w in [40usize, 58, 80, 120] {
            let rows = spend_rows(d.live.as_ref().unwrap(), d.hard.as_ref(), w, &p);
            assert_eq!(rows.len(), 2, "a line went missing at {}", w);
            let plain: Vec<String> = rows.iter().map(|r| strip(r)).collect();
            for row in &plain {
                assert!(row.chars().count() <= w - 1, "row {} wide at {}: {:?}", row.chars().count(), w, row);
            }
            // Right-aligned money lines up the decimal points and the
            // `of` after them - not the dollar signs, which sit where each
            // figure's own length puts them.
            let at = |row: &str| row.find(" of ").expect("a denominator");
            assert_eq!(at(&plain[0]), at(&plain[1]), "the amounts stepped at {}: {:?}", w, plain);
        }
        // Wide enough, and the cycle end is there; narrow, and it is not -
        // rather than being cut in half by the clip.
        let wide = spend_rows(d.live.as_ref().unwrap(), d.hard.as_ref(), 100, &p).join("\n");
        assert!(wide.contains("resets "), "{}", wide);
        let narrow = spend_rows(d.live.as_ref().unwrap(), d.hard.as_ref(), 58, &p).join("\n");
        assert!(!narrow.contains("resets"), "a date was clipped rather than dropped: {}", narrow);
        assert!(narrow.contains("$40.36 left"), "the money stood down first: {}", narrow);
    }

    // The unified Cursor and Grok plan, as measured on 2026-10-13: the same
    // three calls in their merged shape, and the two the plan added. Field
    // shapes and figures only - nothing here names the account.

    /// GetCurrentPeriodUsage on the unified plan, around now: the cycle is a
    /// week, the spend fields are gone, and `apiPercentUsed` sits at nought
    /// because third-party models bill to credits instead.
    fn unified_live(limit_set: bool) -> serde_json::Value {
        let start = ((now() - 86400.0) * 1000.0) as i64;
        let end = ((now() + 6.0 * 86400.0) * 1000.0) as i64;
        let spend = if limit_set {
            serde_json::json!({"individualLimit": 10000, "individualRemaining": 10000, "limitType": "user"})
        } else {
            serde_json::json!({"limitType": "user"})
        };
        serde_json::json!({
            "billingCycleStart": start.to_string(),
            "billingCycleEnd": end.to_string(),
            "planUsage": {
                "remainingBonus": false,
                "autoPercentUsed": 5.052802599512591,
                "apiPercentUsed": 0,
                "totalPercentUsed": 5.052802599512591
            },
            "spendLimitUsage": spend,
            "displayThreshold": 200,
            "enabled": true,
            "displayMessage": "You've used 5% of your included usage",
        })
    }

    fn unified_plan() -> serde_json::Value {
        let month_end = ((now() + 30.0 * 86400.0) * 1000.0) as i64;
        serde_json::json!({
            "planInfo": {
                "planName": "Ultra", "price": "",
                "billingCycleEnd": month_end.to_string(),
                "includedUsagePeriod": "INCLUDED_USAGE_PERIOD_WEEKLY",
                "includedUsageResetsAt": "0"
            },
            "isMerged": true
        })
    }

    fn unified_prepaid() -> serde_json::Value {
        serde_json::json!({
            "billingMode": "PREPAID_BILLING_MODE_PREPAID",
            "eligible": true,
            "availableCents": "2000",
            "fundedCents": "2000",
            "autoTopUp": {"enabled": true, "thresholdCents": "200", "topUpAmountCents": "1000"},
            "topUps": [{"origin": "TOPUP_MANUAL", "amountCents": "2000", "status": "SUCCEEDED"}],
            "hasPaymentMethod": true,
            "spentThisMonthCents": "0"
        })
    }

    fn unified_month() -> serde_json::Value {
        let start = ((now() - 86400.0) * 1000.0) as i64;
        let end = ((now() + 29.0 * 86400.0) * 1000.0) as i64;
        serde_json::json!({"startDateEpochMillis": start.to_string(), "endDateEpochMillis": end.to_string()})
    }

    pub(super) fn unified_data(limit_set: bool) -> Data {
        Data {
            live: Some(unified_live(limit_set)),
            plan: Some(unified_plan()),
            hard: Some(serde_json::json!({"hardLimit": if limit_set { 100 } else { 2147483647i64 }})),
            prepaid: Some(unified_prepaid()),
            month: Some(unified_month()),
            sand: Some(serde_json::json!({
                "hasNonZeroIncludedLimit": true, "usagePercent": 4.969027, "isUnified": true
            })),
            activity: Some(unified_activity()),
            ..Data::default()
        }
    }

    /// GetViewableProfileByHandle's activitySummary, in the measured shape:
    /// counts as numbers with a string twin, and the streaks beside them.
    fn unified_activity() -> serde_json::Value {
        let day = |back: i64| (Local::now().date_naive() - Days::days(back)).format("%Y-%m-%d").to_string();
        serde_json::json!({
            "longestStreak": 33, "currentStreak": 2,
            "activityCounts": [
                {"date": day(2), "count": 568088972, "value": "568088972"},
                {"date": day(1), "count": 179410954, "value": "179410954"},
                {"date": day(9), "value": "25164817"},
            ]
        })
    }

    #[test]
    fn the_plan_says_whether_it_is_unified_and_either_marker_is_enough() {
        assert!(is_unified(Some(&unified_plan())));
        let mut merged_only = unified_plan();
        merged_only["planInfo"].as_object_mut().unwrap().remove("includedUsagePeriod");
        assert!(is_unified(Some(&merged_only)), "isMerged alone");
        let mut period_only = unified_plan();
        period_only.as_object_mut().unwrap().remove("isMerged");
        assert!(is_unified(Some(&period_only)), "includedUsagePeriod alone");
        // An account on its own plan carries neither.
        let own = serde_json::json!({"planInfo": {"planName": "Ultra", "includedAmountCents": 40000}});
        assert!(!is_unified(Some(&own)));
        assert!(!is_unified(None), "no plan read is not a unified plan");
    }

    /// The SpaceXSI section as plain text, at width `w`.
    fn report(d: &Data, w: usize) -> String {
        cursor_quota(d, w, &palette()).iter().map(|r| strip(r)).collect::<Vec<_>>().join("\n")
    }

    /// Whether the row starting with `label` carries a bar.
    fn has_bar(text: &str, label: &str) -> bool {
        text.lines()
            .find(|l| l.trim_start().starts_with(label))
            .is_some_and(|l| l.contains('░') || l.contains('█'))
    }

    #[test]
    fn the_no_limit_sentinel_is_no_limit_and_never_money() {
        // INT32_MAX was drawn as a cap of $2,147,483,647. It is GetHardLimit
        // saying no limit is set - on either plan.
        let state = extra_state(&unified_live(false), Some(&serde_json::json!({"hardLimit": 2147483647i64})));
        assert_eq!(state, ExtraState::Unlimited { used: 0.0 });
        assert_eq!(extra_lane_pct(&state), None, "a percentage of no limit");
        let mut own = measured();
        own["spendLimitUsage"] = serde_json::json!({"limitType": "user"});
        assert_eq!(
            extra_state(&own, Some(&serde_json::json!({"hardLimit": "2147483647"}))),
            ExtraState::Unlimited { used: 0.0 },
            "the sentinel on an account's own plan, as a string"
        );
        for w in [40usize, 80, 120] {
            let rows = cursor_tab(&unified_data(false), w, &palette()).join("\n");
            assert!(!rows.contains("21474836"), "the sentinel reached the screen at {w}: {rows}");
        }
        // No limit and nothing spent: the account is not using one, so the
        // report says nothing about it at all.
        let text = report(&unified_data(false), 120);
        assert!(!text.contains("usage limit"), "{text}");
        // No limit with money spent: the spend, with no bar.
        let mut d = unified_data(false);
        d.prepaid.as_mut().unwrap()["spentThisMonthCents"] = serde_json::json!("350");
        let text = report(&d, 120);
        assert!(squeeze(&text).contains("usage limit no limit set"), "{text}");
        assert!(squeeze(&text).contains("$3.50 this month"), "{text}");
        assert!(!has_bar(&text, "usage limit"), "a bar of no limit: {text}");
    }

    #[test]
    fn the_unified_tab_draws_one_pool_under_spacexsi_and_each_bar_leads_its_money() {
        let d = unified_data(true);
        let text = report(&d, 120);
        for want in [
            "SpaceXSI", "weekly", "resets in", "of the week gone", "weekly pool",
            "EXTRA USAGE", "month",
            "$20.00 left of $20.00 paid · $0.00 used",
            "auto top-up $10.00 when below $2.00 · payment method saved",
            "top-ups", "$20.00 manual",
            "$0.00 of $100.00 this month · $100.00 left · resets",
        ] {
            assert!(squeeze(&text).contains(want), "missing {want:?}: {text}");
        }
        // Each bar leads its block, and its dollars and refill rule sit
        // under it rather than above.
        assert!(has_bar(&text, "prepaid credits") && has_bar(&text, "usage limit"), "{text}");
        let lines: Vec<&str> = text.lines().collect();
        let at = |needle: &str| lines.iter().position(|l| l.contains(needle)).expect(needle);
        assert!(at("prepaid credits") < at("left of $20.00 paid"), "{text}");
        assert!(at("left of $20.00 paid") < at("auto top-up"), "{text}");
        assert!(at("auto top-up") < at("usage limit"), "{text}");
        assert!(at("usage limit") < at("of $100.00 this month"), "{text}");
        for gone in ["cursor models", "other models", "grok bot", "── QUOTA", "extra usage "] {
            assert!(!text.contains(gone), "{gone:?} drawn on the unified plan: {text}");
        }
    }

    #[test]
    fn a_bar_shows_only_when_its_feature_is_in_use() {
        // Nothing paid in and no limit set: neither bar, neither block.
        let mut d = unified_data(false);
        d.prepaid = Some(serde_json::json!({"billingMode": "PREPAID_BILLING_MODE_PREPAID", "eligible": true}));
        let text = report(&d, 120);
        assert!(!text.contains("prepaid credits") && !text.contains("usage limit"), "{text}");
        assert!(!text.contains("auto top-up"), "a refill rule for credits nobody bought: {text}");
        let labels: Vec<String> = lanes(&d).into_iter().map(|l| l.label).collect();
        assert_eq!(labels, ["weekly pool"]);
    }

    #[test]
    fn additional_usage_resets_with_the_month_not_the_week() {
        let d = unified_data(true);
        let text = report(&d, 120);
        let month_end = loose(&d.month.as_ref().unwrap()["endDateEpochMillis"]).unwrap();
        let week_end = loose(&d.live.as_ref().unwrap()["billingCycleEnd"]).unwrap();
        let line = text.lines().find(|l| l.contains("this month")).expect("a this-month line");
        assert!(line.contains(&format!("resets {}", day_month(month_end))), "{line}");
        assert!(!line.contains(&format!("resets {}", day_month(week_end))), "the weekly reset: {line}");
        // And the summary lane is on the same monthly clock.
        let extra = lanes(&d).into_iter().find(|l| l.label == "usage limit").expect("a usage limit lane");
        assert_eq!(extra.reset, Some(month_end / 1000.0));
    }

    #[test]
    fn the_unified_summary_has_the_pool_and_only_the_bars_in_use_with_no_dollars() {
        let got = lanes(&unified_data(true));
        let labels: Vec<&str> = got.iter().map(|l| l.label.as_str()).collect();
        assert_eq!(labels, ["weekly pool", "usage limit", "prepaid credits"], "{labels:?}");
        assert!(labels.iter().all(|l| !l.contains('$')), "a dollar figure on [+]: {labels:?}");
        let pool = &got[0];
        assert!((pool.pct - 5.052802599512591).abs() < 1e-9);
        assert_eq!(pool.window_secs, Some(7.0 * 86400.0), "the pool is weekly");
        let credits = &got[2];
        assert_eq!(credits.pct, 0.0, "nothing drawn from $20 of $20");
        assert_eq!((credits.window_secs, credits.reset), (None, None), "a balance has no clock");
        // No limit set: no limit bar.
        let labels: Vec<String> = lanes(&unified_data(false)).into_iter().map(|l| l.label).collect();
        assert_eq!(labels, ["weekly pool", "prepaid credits"]);
        // Half the balance spent is half the bar.
        let mut d = unified_data(true);
        d.prepaid.as_mut().unwrap()["availableCents"] = serde_json::json!("1000");
        let credits = lanes(&d).into_iter().find(|l| l.label == "prepaid credits").unwrap();
        assert_eq!(credits.pct, 50.0);
    }

    /// The measured pre-merge account: no unified marker anywhere.
    pub(super) fn own_plan_data() -> Data {
        Data {
            live: Some(measured()),
            hard: Some(hard_fifty()),
            plan: Some(serde_json::json!({"planInfo": {"planName": "Ultra", "includedAmountCents": 40000}})),
            ..Data::default()
        }
    }

    #[test]
    fn an_account_on_its_own_plan_still_draws_all_three_lanes() {
        // Nothing on its tab or its summary moves.
        let d = own_plan_data();
        let labels: Vec<String> = lanes(&d).into_iter().map(|l| l.label).collect();
        assert_eq!(labels, ["total", "cursor models", "other models", "extra $50"]);
        let joined = cursor_quota(&d, 120, &palette()).iter().map(|r| strip(r)).collect::<Vec<_>>().join("\n");
        assert!(joined.contains("QUOTA") && !joined.contains("SpaceXSI"), "{joined}");
        assert!(joined.contains("extra usage"), "{joined}");
    }

    #[test]
    fn a_unified_plan_whose_money_calls_failed_keeps_the_pool_and_guesses_no_balance() {
        let mut d = unified_data(true);
        d.prepaid = None;
        d.month = None;
        let text = report(&d, 120);
        assert!(text.contains("weekly pool"), "{text}");
        assert!(squeeze(&text).contains("prepaid credits not reported"), "a failed call is not 'unused': {text}");
        assert!(!text.contains("$20.00"), "a balance nobody read: {text}");
        // The limit still stands on the usage block, and its reset falls
        // back to the plan's own monthly end rather than the week.
        let line = text.lines().find(|l| l.contains("this month")).expect("a this-month line");
        let plan_end = loose(&d.plan.as_ref().unwrap()["planInfo"]["billingCycleEnd"]).unwrap();
        assert!(line.contains(&format!("resets {}", day_month(plan_end))), "{line}");
    }

    #[test]
    fn an_empty_balance_with_no_top_up_says_so() {
        let mut d = unified_data(true);
        d.prepaid = Some(serde_json::json!({
            "billingMode": "PREPAID_BILLING_MODE_PREPAID",
            "fundedCents": "2000",
            "autoTopUp": {"enabled": false},
        }));
        let text = report(&d, 120);
        assert!(text.contains("$0.00 left"), "an omitted balance is nought on a prepaid account: {text}");
        assert!(text.contains("· empty"), "{text}");
        assert!(squeeze(&text).contains("auto top-up off"), "{text}");
    }

    #[test]
    fn the_top_ups_and_a_rule_that_cannot_run_are_on_the_tab() {
        let mut d = unified_data(true);
        d.prepaid = Some(serde_json::json!({
            "billingMode": "PREPAID_BILLING_MODE_PREPAID",
            "availableCents": "150", "fundedCents": "2000",
            "autoTopUp": {"enabled": true, "thresholdCents": "200", "topUpAmountCents": "1000"},
            "hasPaymentMethod": false,
            "topUps": [
                {"origin": "TOPUP_MANUAL", "amountCents": "2000", "status": "SUCCEEDED", "createdAtMs": "1000"},
                {"origin": "TOPUP_AUTO", "amountCents": "1000", "status": "FAILED", "createdAtMs": "2000"}
            ]
        }));
        let text = report(&d, 160);
        assert!(text.contains("no payment method to charge"), "{text}");
        let lines: Vec<String> = text.lines().skip_while(|l| !l.contains("top-ups")).take(2).map(squeeze).collect();
        assert!(lines[0].contains("$10.00 auto failed"), "the newest first: {lines:?}");
        assert!(lines[1].contains("$20.00 manual"), "{lines:?}");
        assert!(squeeze(&text).contains("$1.50 left of $20.00 paid · $18.50 used"), "{text}");
        assert!(!text.contains(" ? "), "a refill with no time printed a corrupt date: {text}");
    }

    #[test]
    fn the_report_drops_whole_parts_rather_than_cutting_them() {
        // `seg` keeps every row inside its pane by clipping, which passes a
        // width check and still leaves "13 Oct → 1" and "payment metho" on
        // screen. Each part has to fit whole or stand down.
        let d = unified_data(true);
        let end = day_month(loose(&d.month.as_ref().unwrap()["endDateEpochMillis"]).unwrap());
        for w in [40usize, 50, 58, 64, 80, 120] {
            let rows: Vec<String> = cursor_quota(&d, w, &palette()).iter().map(|r| strip(r)).collect();
            let top = &rows[0];
            assert!(!top.trim_end().ends_with("resets") && !top.trim_end().ends_with("resets in"), "a cut countdown at {w}: {top:?}");
            let head = rows.iter().find(|r| r.contains("EXTRA USAGE")).expect("a heading");
            if head.contains('→') {
                assert!(head.contains(&end), "a cut month at {w}: {head:?}");
            }
            let balance = rows.iter().find(|r| r.contains("$20.00 left")).expect("a balance line");
            let b = balance.trim_end();
            assert!(b.ends_with("left") || b.ends_with("paid") || b.ends_with("used") || b.ends_with("empty"), "a cut tail at {w}: {balance:?}");
            let rule = rows.iter().find(|r| r.contains("auto top-up")).expect("a rule line");
            if rule.contains("payment") {
                assert!(rule.contains("payment method saved"), "a cut phrase at {w}: {rule:?}");
            }
            let month = rows.iter().find(|r| r.contains("of $100.00")).expect("a month line");
            let m = month.trim_end();
            assert!(
                m.ends_with("$100.00") || m.ends_with("this month") || m.ends_with("left") || m.chars().rev().take(4).all(|c| c.is_ascii_alphanumeric() || c == ' '),
                "a cut tail at {w}: {month:?}"
            );
            assert!(!m.ends_with("this mon") && !m.ends_with("lef") && !m.ends_with("rese"), "a cut tail at {w}: {month:?}");
        }
        let mut d = unified_data(true);
        d.prepaid.as_mut().unwrap()["hasPaymentMethod"] = serde_json::json!(false);
        for w in [40usize, 58, 120] {
            let text = report(&d, w);
            assert!(text.contains("no payment method to charge"), "the warning was dropped at {w}: {text}");
        }
    }

    #[test]
    fn the_activity_heatmap_draws_the_profiles_days_and_streaks() {
        let d = unified_data(true);
        let days = activity_days(d.activity.as_ref().unwrap());
        assert_eq!(days.len(), 3);
        assert!(days.values().any(|v| *v == 25164817.0), "a count stated only as a string");
        let text = report(&d, 120);
        assert!(squeeze(&text).contains("ACTIVITY ── profile tokens / day · peak"), "{text}");
        assert!(squeeze(&text).contains("streak 2 days · longest 33"), "{text}");
        for w in [40usize, 57, 58] {
            let head = report(&d, w).lines().find(|l| l.contains("ACTIVITY")).unwrap().trim_end().to_string();
            assert!(!head.ends_with(" on") && !head.ends_with(" o"), "a cut date at {w}: {head:?}");
        }
        assert!(text.contains("Less") && text.contains("More"), "{text}");
        // Its plot is published at its own row.
        let mut plots = Vec::new();
        let rows = cursor_quota_with_plots(&d, 120, &palette(), &mut plots);
        let head = rows.iter().position(|r| strip(r).contains("ACTIVITY")).unwrap();
        assert!(plots.iter().any(|pl| pl.y > head), "no calendar plot under the heading");
        // A failed call says so; a profile with no days says that.
        let mut d = unified_data(true);
        d.activity = None;
        assert!(squeeze(&report(&d, 120)).contains("ACTIVITY ── not reported"));
        d.activity = Some(serde_json::json!({}));
        assert!(squeeze(&report(&d, 120)).contains("no active days on the profile"));
    }

    #[test]
    fn a_spacexsi_cursor_tab_draws_no_local_authorship() {
        let mut d = unified_data(true);
        d.ok = true;
        d.hashes = 46_404;
        d.lines = 1_000;
        let text = cursor_tab(&d, 120, &palette()).iter().map(|r| strip(r)).collect::<Vec<_>>().join("\n");
        for local in ["AI-WRITTEN CODE", "WHO WROTE IT", "tracked edits", "No AI-written-code counts"] {
            assert!(!text.contains(local), "local data {local:?} under SpaceXSI: {text}");
        }
        // Its own plan still draws them.
        let mut own = own_plan_data();
        own.ok = true;
        own.hashes = 46_404;
        assert!(cursor_tab(&own, 120, &palette()).join("\n").contains("AI-WRITTEN CODE"));
    }

    #[test]
    fn a_cached_window_past_its_reset_is_rolled_before_it_is_matched() {
        // Review: Grok's ended window was rolled and Cursor's was not, so a
        // cached Cursor response across the reset split one account in two.
        let mut d = unified_data(true);
        let week = 7.0 * 86_400_000.0;
        let end = now() * 1000.0 - 60_000.0;
        d.live.as_mut().unwrap()["billingCycleStart"] = serde_json::json!((end - week).to_string());
        d.live.as_mut().unwrap()["billingCycleEnd"] = serde_json::json!(end.to_string());
        let (s, e) = pool_window(&d).unwrap();
        assert_eq!((s, e), (end, end + week));
        // A window still running is taken as sent.
        let d = unified_data(true);
        let live = d.live.as_ref().unwrap();
        let sent = (loose(&live["billingCycleStart"]).unwrap(), loose(&live["billingCycleEnd"]).unwrap());
        if sent.1 > now() * 1000.0 {
            assert_eq!(pool_window(&d), Some(sent));
        }
    }

    #[test]
    fn narrow_panes_keep_whole_figures_and_the_pool_percentage() {
        // Review: money in the report was clipped (`$0.00 of $1`) and the
        // pool's percentage pushed off the row on narrow panes.
        let d = unified_data(true);
        for w in [26usize, 30, 40, 50] {
            let text = report(&d, w);
            let pool = text.lines().find(|l| l.trim_start().starts_with("weekly pool")).unwrap();
            assert!(pool.contains("5.1%"), "the pool's percentage was cut at {w}: {pool:?}");
            assert!(text.contains("$0.00 of $100.00"), "the limit was cut at {w}: {text}");
            assert!(text.contains("$20.00 left"), "the balance was cut at {w}: {text}");
            // Review: the refill rule is money too, and wraps whole parts
            // onto the next row rather than end in `when be`.
            for part in ["auto top-up", "$10.00", "when below", "$2.00"] {
                assert!(text.contains(part), "the refill rule's {part:?} was cut at {w}: {text}");
            }
            for row in text.lines() {
                assert!(row.chars().count() <= w - 1, "{w}: {row:?}");
            }
        }
    }

    #[test]
    fn a_team_pool_keeps_the_pools_spend() {
        // Review: on a team limit the account's own spentThisMonthCents
        // replaced the pool's total - one member's share shown as the whole.
        let mut d = unified_data(true);
        d.live.as_mut().unwrap()["spendLimitUsage"] =
            serde_json::json!({"individualLimit": 10000, "totalSpend": 5000, "limitType": "team"});
        d.prepaid.as_mut().unwrap()["spentThisMonthCents"] = serde_json::json!("100");
        let state = additional_state(&d, d.live.as_ref().unwrap());
        assert_eq!(state, ExtraState::Fixed { used: 5000.0, limit: 10000.0, shared: true });
        // A user limit still takes the account's own figure.
        let mut d = unified_data(true);
        d.prepaid.as_mut().unwrap()["spentThisMonthCents"] = serde_json::json!("350");
        assert_eq!(
            additional_state(&d, d.live.as_ref().unwrap()),
            ExtraState::Fixed { used: 350.0, limit: 10000.0, shared: false }
        );
    }

    #[test]
    fn the_report_bars_keep_their_percentage_on_narrow_panes() {
        // Review: the report bars kept an eight-cell floor and clipped
        // their own percentage at 26 to 40 columns.
        let d = unified_data(true);
        for w in [26usize, 30, 34, 40] {
            let text = report(&d, w);
            for label in ["prepaid credits", "usage limit"] {
                let row = text.lines().find(|l| l.trim_start().starts_with(label)).unwrap_or_else(|| panic!("{label} at {w}: {text}"));
                assert!(row.trim_end().ends_with('%'), "{label}'s percentage cut at {w}: {row:?}");
                assert!(row.chars().count() <= w - 1, "{w}: {row:?}");
            }
        }
    }

    #[test]
    fn every_unified_row_fits_its_pane() {
        for limit_set in [true, false] {
            for place in [Spacexsi::Alone, Spacexsi::Shared, Spacexsi::Separate] {
                let mut d = unified_data(limit_set);
                d.spacexsi = place;
                for w in [26usize, 30, 40, 58, 80, 120] {
                    for row in cursor_tab(&d, w, &palette()) {
                        let n = strip(&row).chars().count();
                        assert!(n <= w - 1, "{n} wide at {w} ({place:?}): {:?}", strip(&row));
                    }
                }
            }
        }
    }

    #[test]
    fn the_pool_bar_splits_by_product_and_the_separate_heading_says_via_cursor() {
        let mut d = unified_data(true);
        d.spacexsi = Spacexsi::Shared;
        d.pool_split = vec![("Cursor".into(), 3.0), ("Grok Bot".into(), 2.0)];
        let text = report(&d, 120);
        let lines: Vec<String> = text.lines().map(squeeze).collect();
        let bar = lines.iter().position(|l| l.starts_with("weekly pool")).expect("a pool bar");
        // Five of a hundred is a twentieth of the bar; the rest is track.
        let raw = text.lines().find(|l| l.trim_start().starts_with("weekly pool")).unwrap();
        let (solid, track) = (raw.matches('█').count(), raw.matches('░').count());
        assert!(track > solid, "the unused week drawn solid: {raw:?}");
        // And the row runs to the pane's edge.
        assert_eq!(raw.trim_end().chars().count(), raw.chars().count());
        assert!(raw.chars().count() >= 115, "the bar did not fill the row: {} of 119", raw.chars().count());
        assert_eq!(lines[bar + 1], "■ Cursor ~3%", "{text}");
        assert_eq!(lines[bar + 2], "■ Grok Bot ~2%", "{text}");
        // Text mode redraws a bar from its plot's own cells over the row, so
        // those must carry the track too - the row alone passed while the
        // pane drew the whole week solid.
        let mut plots = Vec::new();
        let rows = cursor_quota_with_plots(&d, 120, &palette(), &mut plots);
        let at = rows.iter().position(|r| strip(r).trim_start().starts_with("weekly pool")).unwrap();
        let plot = plots.iter().find(|pl| pl.y == at).expect("the pool bar's plot");
        let cells: String = plot.text_row(0).iter().map(|(_, t)| t.as_str()).collect();
        assert!(cells.matches('░').count() > cells.matches('█').count(), "{cells:?}");
        // No split to hand: the plain paced bar, and no product lines.
        d.pool_split.clear();
        assert!(!report(&d, 120).contains('■'));
        d.spacexsi = Spacexsi::Separate;
        assert!(report(&d, 160).contains("SpaceXSI ── via Cursor"));
        // Every product line keeps its share on a narrow pane.
        let mut narrow = unified_data(true);
        narrow.pool_split = vec![("Cursor".into(), 5.0), ("Grok Bot".into(), 5.0)];
        for w in [26usize, 30] {
            let text = report(&narrow, w);
            assert!(text.contains("Cursor ~5%") || squeeze(&text).contains("■ Cursor ~5%"), "share cut at {w}: {text}");
            assert!(squeeze(&text).contains("■ Grok Bot ~5%"), "share cut at {w}: {text}");
        }
        // Every product line fits a narrow pane whole.
        let mut d = unified_data(true);
        d.pool_split = vec![("Cursor".into(), 5.0), ("Grok Bot".into(), 5.0), ("Grok Imagine".into(), 1.0)];
        for w in [30usize, 40, 58] {
            for row in cursor_quota(&d, w, &palette()) {
                assert!(strip(&row).chars().count() <= w - 1, "{w}: {:?}", strip(&row));
            }
        }
    }
}

/// Readings for tests outside this module, which cannot reach the private
/// fields: the measured unified account, and the measured account on its
/// own plan.
#[cfg(test)]
impl Data {
    pub fn unified_fixture(limit_set: bool) -> Data {
        tests::unified_data(limit_set)
    }

    pub fn own_plan_fixture() -> Data {
        tests::own_plan_data()
    }

    /// The same reading with the usage call answered but no pool figure in
    /// it - neither the total nor the auto percentage.
    pub fn without_pool_figure(mut d: Data) -> Data {
        if let Some(plan) = d.live.as_mut().and_then(|l| l["planUsage"].as_object_mut()) {
            plan.remove("totalPercentUsed");
            plan.remove("autoPercentUsed");
        }
        d
    }

    /// The same reading with the usage call unanswered.
    /// The same reading cached from a week that has since ended.
    pub fn from_an_ended_week(mut d: Data) -> Data {
        if let Some(live) = d.live.as_mut() {
            let end = now() * 1000.0 - 60_000.0;
            live["billingCycleStart"] = serde_json::json!((end - 7.0 * 86_400_000.0).to_string());
            live["billingCycleEnd"] = serde_json::json!(end.to_string());
        }
        d
    }

    pub fn without_live(mut d: Data) -> Data {
        d.live = None;
        d.live_why = "Cursor's usage endpoint did not answer".into();
        d
    }
}

#[cfg(test)]
fn cursor_spend_rows(d: &Data, w: usize, p: &Palette) -> Vec<String> {
    cursor_spend_with_plots(d, w, p, &mut Vec::new())
}

#[cfg(test)]
fn cursor_quota(d: &Data, w: usize, p: &Palette) -> Vec<String> {
    cursor_quota_with_plots(d, w, p, &mut Vec::new())
}

#[cfg(test)]
fn cursor_tab(d: &Data, w: usize, p: &Palette) -> Vec<String> {
    cursor_tab_with_plots(d, w, p, &mut Vec::new())
}
