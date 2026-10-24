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

//! Grok: what its transcripts spent, and the quota that arrives elsewhere.
//!
//! Grok writes a running totalTokens on every session event. Deltas between
//! consecutive events, bucketed by the event's own timestamp, give the
//! per-day figures - the running total alone would credit an entire session
//! to whichever day it happened to be read on.

use std::collections::HashMap;

use chrono::{Datelike, Local, NaiveDate, TimeZone};
use opscope_core as tc;

use crate::shared::*;
use crate::*;

/// Where the CLI keeps its transcripts. Nested a couple of levels down, so
/// it is walked rather than listed.
const SESSIONS: &str = ".grok/sessions";
/// The quota is not in the session transcripts: it arrives on the client
/// log, which the CLI writes as it talks to the server.
const LOG: &str = ".grok/logs/unified.jsonl";
/// Where the CLI leaves the token it authenticates with.
const AUTH: &str = ".grok/auth.json";
/// Seconds to wait on the billing call before falling back to the log.
const QUOTA_TIMEOUT: u64 = 6;
/// The CLI, whose only job here is to refresh the token it owns.
const CLI: &str = ".grok/bin/grok";
/// Cache key for the billing reading, per signed-in account: a reading
/// held for one account must not answer for another, whose plan markers
/// and window would then decide where SpaceXSI is drawn.
fn ping_key(account: Option<&str>) -> String {
    format!("grok:billing:{}", account.unwrap_or_default())
}
/// Cache key for the last session-end refresh, so one ending refreshes once.
const SEEN_KEY: &str = "grok:session-seen";
/// Cache key for the last expiry a refresh was attempted against, and
/// whether that attempt was made before or after the lapse.
const EXPIRY_KEY: &str = "grok:token-expiry";
/// Cache key for the last reading the server actually gave, kept apart
/// from `ping_key` because that slot is overwritten by every refusal and
/// emptied by every token refresh. This one only ever holds a success,
/// tagged with the token's account so a sign-in as someone else does not
/// keep showing the previous account's figure.
const LIVE_KEY: &str = "grok:last-live";
/// Refresh this long before the token actually lapses, so the ask that
/// follows is not the one that discovers it has.
const TOKEN_MARGIN: f64 = 600.0;
/// How long a session must be quiet before it counts as over. Long enough
/// that a pause for thought is not an ending.
const SESSION_QUIET: f64 = 120.0;
/// And how long before it is old news. Launching the widget days after the
/// last session should not start the CLI to refresh a window nobody is
/// watching.
const SESSION_STALE: f64 = 6.0 * 3600.0;
/// The CLI's own billing endpoint. Same reading its `/usage` shows, and the
/// only account-wide source Grok has that is not a file on this disk.
const BILLING: &str = "https://cli-chat-proxy.grok.com/v1/billing?format=credits";
/// The credit reading is one line among the client log's chatter and is
/// only rewritten when the server sends a new one, so the tail has to be
/// long enough to still contain one after a busy session.
const LOG_TAIL: u64 = 2 * 1024 * 1024;

/// Grok's credit window, as its own CLI receives it.
///
/// Not hidden and not inferred: the server sends it and the CLI writes it
/// into the log under `.ctx.config`. An earlier pass in the Python
/// concluded no quota existed, having grepped for limit/quota/remaining/
/// reset - the keys are `creditUsagePercent` and `currentPeriod`, so the
/// search missed them and the tab said so in print for a day.
#[derive(Clone, Default)]
struct Quota {
    pct: Option<f64>,
    /// The same week split by product, where the answer carries it:
    /// GrokBuild, GrokChat, GrokImagine. Empty when it does not, which is
    /// every reading the log recorded - this arrived with the credits
    /// endpoint and the log lines predate it.
    products: Vec<(String, f64)>,
    /// When this reading was taken, as epoch seconds - the log line's own
    /// `ts`, or the moment of the fetch. Not when it was read: the tab
    /// judges a reading by its age, and those are different numbers.
    taken: Option<f64>,
    kind: String,
    start: String,
    end: String,
    tier: String,
    on_demand_used: Option<f64>,
    on_demand_cap: Option<f64>,
    /// Dollars, converted from the cents x.ai sends. See `prepaid_of`.
    prepaid: Option<f64>,
    /// The answer carries a marker only the unified Cursor and Grok plan
    /// sends. See `unified_markers`.
    unified: bool,
}

/// What the transcripts on this machine recorded, plus the account-wide
/// credit window they say nothing about.
#[derive(Clone, Default)]
pub struct Data {
    ok: bool,
    /// Files that carried a non-zero total, and files looked at.
    sessions: usize,
    files: usize,
    total: f64,
    daily: HashMap<NaiveDate, f64>,
    /// How current the session logs behind `total` and `daily` are.
    pub history: History,
    /// Newest transcript mtime, as epoch seconds.
    last: f64,
    quota: Option<Quota>,
    /// True when the quota came from the server just now rather than from
    /// the log. A number nobody labelled as old reads as current, and this
    /// one was out by more than double the last time it went unlabelled.
    quota_live: bool,
    /// When the server was last asked, as epoch seconds. Zero when it never
    /// has been - the asking is off unless it is turned on.
    quota_at: f64,
    /// Seconds between asks, so the tab can say what the interval is rather
    /// than leaving the reader to find it in a config file.
    quota_every: f64,
    /// Why the figure on screen is not the server's, when it is not. Empty
    /// when it is, or when nothing is asking.
    quota_why: String,
    /// Where this account stands relative to the unified plan, once both
    /// vendors have been read. Set by `vendors::link_spacexsi`.
    pub spacexsi: Spacexsi,
}

/// The integer following `key` on a line.
///
/// The transcripts are one JSON object per event and only two numbers on
/// each are wanted, so the line is scanned rather than parsed. Grok's
/// events carry whole tool results, and building a document out of every
/// one of them to reach two integers costs more than the entire read.
fn int_after(line: &str, key: &str) -> Option<f64> {
    let at = line.find(key)? + key.len();
    let digits: String = line[at..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

/// One transcript's spend, as a total and by day.
///
/// The running counter is followed rather than trusted: a session that
/// resumes replays earlier events, so the count can go backwards, and a
/// decrease is a replay rather than negative usage. Days are UTC because
/// the timestamp is epoch milliseconds and the Python bucketed it that way.
#[cfg(test)]
fn session_days(body: &str) -> (f64, HashMap<String, f64>) {
    let mut lines = SessionLines::default();
    for line in body.lines() {
        lines.line(line);
    }
    (lines.total, lines.days)
}

/// `session_days` a line at a time, carrying the running counter from one
/// line to the next, so `follow` can hand it only what a live session
/// appended since the last pass.
#[derive(Default)]
pub struct SessionLines {
    prev: f64,
    total: f64,
    days: HashMap<String, f64>,
}

impl Persist for SessionLines {
    fn meta(&self) -> serde_json::Value {
        serde_json::json!({"prev": self.prev, "total": self.total, "days": self.days})
    }
    fn records(&self) -> Option<&Records> {
        None
    }
    fn load(v: &serde_json::Value, _: Records) -> Option<SessionLines> {
        let mut days = HashMap::new();
        for (day, n) in v["days"].as_object()? {
            days.insert(day.clone(), n.as_f64()?);
        }
        Some(SessionLines { prev: v["prev"].as_f64()?, total: v["total"].as_f64()?, days })
    }
}

impl Lines for SessionLines {
    fn line(&mut self, line: &str) {
        let Some(value) = int_after(line, "\"totalTokens\":") else {
            return;
        };
        let step = value - self.prev;
        self.prev = self.prev.max(value);
        if step <= 0.0 {
            return;
        }
        // A step whose event carries no timestamp is dropped rather than
        // banked against whichever day was read last: a calendar that
        // invents a day is worse than one that is short by a step.
        let Some(ms) = int_after(line, "\"agentTimestampMs\":") else {
            return;
        };
        // Local, not UTC, because Claude, Codex and Cursor all bucket
        // locally and day_calendar draws the four on one wall under the
        // same headings. The stamp is epoch milliseconds with no zone of
        // its own to honour, so there is nothing here arguing for UTC the
        // way Copilot's reset boundary does.
        let Some(at) = Local.timestamp_millis_opt(ms as i64).single() else {
            return;
        };
        *self.days.entry(at.date_naive().to_string()).or_insert(0.0) += step;
        self.total += step;
    }
}

/// A credit figure from the log, which arrives as `{"val": n}`.
///
/// Read as a number or as a string, because the server writes int64s as
/// strings - JSON has no room for them - and a percentage that arrives
/// quoted would otherwise drop the whole quota block.
fn val_of(value: &serde_json::Value) -> Option<f64> {
    value.as_f64().or_else(|| value.as_str()?.parse().ok())
}

/// The most recent credit reading on the client log.
///
/// The newest *period* wins rather than the newest line: the log repeats
/// the same window on every exchange, and a line further down describing
/// an older window is a reply that was still in flight.
fn newest_quota<'a>(lines: impl Iterator<Item = &'a str>) -> Option<Quota> {
    let mut best: Option<Quota> = None;
    for line in lines {
        // Rejected on a substring first: almost every line of this log is
        // something else, and parsing two megabytes of them to find the
        // few that carry a billing reading costs more than the read.
        // Unified-billing accounts omit `creditUsagePercent` entirely, so
        // the paid-allowance keys have to count on their own or a valid
        // on-demand lane never leaves the log.
        // Unified-plan lines can carry neither - nought usage is omitted,
        // and the plan has no x.ai cap - so its own keys admit a line too.
        if !line.contains("creditUsagePercent")
            && !line.contains("onDemandCap")
            && !line.contains("prepaidBalance")
            && !line.contains("isUnifiedBillingUser")
        {
            continue;
        }
        let Ok(d) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let cfg = &d["ctx"]["config"];
        let period = &cfg["currentPeriod"];
        // The period is what makes this a reading, same as quota_from.
        // An omitted percentage is not a missing reading: the on-demand
        // cap can still be there, and a null percentage still names the
        // tier and the billing period.
        let start = text(period, "start");
        let end = text(period, "end");
        if start.is_empty() || end.is_empty() {
            continue;
        }
        let got = Quota {
            pct: val_of(&cfg["creditUsagePercent"]),
            products: products_of(cfg),
            taken: iso_epoch(&text(&d, "ts")),
            kind: text(period, "type"),
            start,
            end,
            tier: text(&d["ctx"], "subscriptionTier"),
            on_demand_used: val_of(&cfg["onDemandUsed"]["val"]),
            on_demand_cap: val_of(&cfg["onDemandCap"]["val"]),
            prepaid: prepaid_of(cfg),
            unified: unified_markers(cfg),
        };
        let better = match &best {
            None => true,
            Some(had) => got.start >= had.start,
        };
        if better {
            best = Some(got);
        }
    }
    best
}

/// What every transcript here spent, and what the server last said about
/// the account's credits.
/// The server if it is allowed and will answer, the log if not.
///
/// Held between asks rather than asked on every frame: the pane redraws
/// every thirty seconds and this window moves over days, so the interval is
/// the configured one and the reading in between is the one already had.
/// The credit window, whether it came from the server, when it was asked
/// for, and - when it did not - which of the four reasons applies.
fn quota_now(caches: &mut Caches, cfg: &Config) -> (Option<Quota>, bool, f64, String) {
    let from_log = || {
        newest_quota(tail_lines(&under_home(LOG), LOG_TAIL).iter().map(String::as_str))
    };
    if !cfg.grok_ping {
        return (from_log(), false, 0.0, String::new());
    }
    // What to show when the server cannot be asked or does not answer: the
    // fresher of the last thing it said and the last thing the log
    // recorded. The log used to win outright, and on this machine that
    // put a figure from twenty-seven days back over one from two hours
    // back, with nothing on the row to say which was which.
    let account = token_account();
    let ping = ping_key(account.as_deref());
    let held = last_live_of(caches, account.as_deref());
    let held_at = held.as_ref().and_then(|q| q.taken).unwrap_or(0.0);
    let key = match usable_token() {
        Ok(k) => k,
        Err(why) => {
            // The widget's own refresh has already been tried after the
            // lapse and did not move the expiry: the CLI needs a real
            // sign-in, and saying "the CLI refreshes it" again would send
            // the reader to the thing that just failed.
            let why = if refresh_gave_up(caches) {
                format!("{} - it did not; run grok to sign in again", why)
            } else {
                why
            };
            // "polled x.ai" is when we last asked, not when a success
            // last landed. A failed poll still updates the ping slot; a
            // refresh that has just run clears it, and then the last
            // live reading's time is what remains.
            let at = caches
                .live
                .get(&ping)
                .map(|(when, _, _)| *when)
                .unwrap_or(held_at);
            return (fresher(held, from_log()), false, at, why);
        }
    };
    let ttl = ping_ttl(cfg.grok_ping_minutes);
    let got = cached(caches, &ping, ttl, || fetch_billing(&key, QUOTA_TIMEOUT));
    // When the ask was actually made, which is not this frame most of the
    // time. The tab reports it, so it has to be the fetch and not the read.
    let at = caches.live.get(&ping).map(|(when, _, _)| *when).unwrap_or(0.0);
    match got.as_ref() {
        None => (
            fresher(held, from_log()),
            false,
            at,
            "x.ai did not answer".to_string(),
        ),
        Some(body) => match quota_from(body) {
            Some(mut q) => {
                let why = String::new();
                q.taken = Some(at);
                caches.live.insert(
                    LIVE_KEY.to_string(),
                    (
                        at,
                        Some(serde_json::json!({
                            "account": account.unwrap_or_default(),
                            "billing": body.clone(),
                        })),
                        f64::MAX,
                    ),
                );
                if q.pct.is_none() {
                    // The server named the window but not the spend. The log
                    // may still hold a percentage, and it is usable only if
                    // it is about this same window: a figure from a window
                    // that has closed is not this one's, however it got here.
                    match from_log() {
                        Some(l) if l.start == q.start && l.end == q.end => {
                            q.pct = l.pct;
                            q.taken = l.taken;
                        }
                        // No reason recorded here on purpose. The row that
                        // stands where the bar would be already says there
                        // is no figure for this period, and the freshness
                        // line saying it too was the same sentence twice.
                        // quota_why is for the cases that line cannot show:
                        // a lapsed token, a refusal, an unreadable answer.
                        _ => {}
                    }
                }
                (Some(q), true, at, why)
            }
            None => (
                fresher(held, from_log()),
                false,
                at,
                "x.ai sent no usable reading".to_string(),
            ),
        },
    }
}

/// The last reading the server gave, dated by when it was fetched, and
/// belonging to the account now on disk.
///
/// Claude Code already guards its cache this way. The billing body names
/// no account, so the marker is borrowed from the token file's map key -
/// issuer and account, which a refresh keeps and a different sign-in
/// does not. A missing marker is not a mismatch: the guard is simply not
/// applied, the same as on a machine whose Claude Code never wrote the uuid.
fn last_live_of(caches: &Caches, account: Option<&str>) -> Option<Quota> {
    let (at, body, _) = caches.live.get(LIVE_KEY)?;
    let body = body.as_ref()?;
    let held = text(body, "account");
    if let (false, Some(now_acct)) = (held.is_empty(), account) {
        if held != now_acct {
            return None;
        }
    }
    let mut q = quota_from(&body["billing"])?;
    q.taken = Some(*at);
    Some(q)
}

/// The more recent of two readings, by when each was taken.
///
/// A reading with no date loses to one with any date: unknown age is not
/// evidence of youth, and the log's lines are dated while a fetch is dated
/// here by the fetch. Ties go to the first, which callers pass the
/// server's reading as.
fn fresher(a: Option<Quota>, b: Option<Quota>) -> Option<Quota> {
    match (a, b) {
        (None, b) => b,
        (a, None) => a,
        (Some(a), Some(b)) => {
            if b.taken.unwrap_or(f64::MIN) > a.taken.unwrap_or(f64::MIN) {
                Some(b)
            } else {
                Some(a)
            }
        }
    }
}

/// Which refresh, if any, the token on disk is due: `Some(false)` for the
/// one made shortly before it lapses, `Some(true)` for the one made after.
///
/// Two, because the CLI only renews a token that has already run out - its
/// refresh is made ahead of a request, on finding the token expired, and a
/// token with ten minutes left is not expired. Measured on 1.0.25: started
/// ten minutes before the lapse it bootstrapped, logged no refresh and
/// left `auth.json` alone; started two hours after, it renewed at once.
/// The early attempt is kept for a CLI that may one day renew ahead of
/// time, and costs one start. The late one is the one that works today.
///
/// `tried` is the last attempt made, as (expiry, after). Each is made once
/// per expiry value and never again, so a login that has genuinely run out
/// costs two starts over the life of its token rather than one every poll.
/// A renewed token has a new expiry and starts the count afresh.
fn refresh_due(expiry: f64, at: f64, tried: Option<(f64, bool)>) -> Option<bool> {
    let after = expiry <= at;
    let before = expiry <= at + TOKEN_MARGIN;
    let done = |phase: bool| tried.is_some_and(|(e, p)| e == expiry && (p == phase || p));
    if after && !done(true) {
        Some(true)
    } else if before && !after && !done(false) {
        Some(false)
    } else {
        None
    }
}

/// The last refresh attempt recorded, as (expiry, after).
fn refresh_tried(caches: &Caches) -> Option<(f64, bool)> {
    let (_, v, _) = caches.live.get(EXPIRY_KEY)?;
    let v = v.as_ref()?;
    Some((v["expiry"].as_f64()?, v["after"].as_bool().unwrap_or(false)))
}

/// True when the token on disk has lapsed and the widget has already run
/// the CLI since it did, to no effect. What is left is a sign-in.
fn refresh_gave_up(caches: &Caches) -> bool {
    let Some(expiry) = token_expiry() else {
        return false;
    };
    expiry <= now() && refresh_tried(caches).is_some_and(|(e, after)| e == expiry && after)
}

/// Run the Grok CLI once, for its side effect: it refreshes the token in
/// `auth.json` on startup, and that token is what the billing request needs.
///
/// Without this the asking works until the token lapses and then quietly
/// stops working, which is the failure it was meant to fix. With it the
/// refresh happens when a session has just ended - the moment the numbers
/// have changed and nobody is at the keyboard waiting.
///
/// It starts somebody else's program, which is part of what grok_ping asks
/// for rather than a setting of its own - polling that stops working the
/// moment the token lapses is not what anybody turned on. The handshake is
/// the smallest one the agent answers: initialize, then close.
fn refresh_token() {
    use std::io::Write;
    let Ok(mut child) = std::process::Command::new(under_home(CLI))
        .args(["agent", "stdio"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    else {
        return;
    };
    if let Some(mut pipe) = child.stdin.take() {
        let _ = pipe.write_all(
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\
              \"params\":{\"protocolVersion\":1,\"clientCapabilities\":{}}}\n",
        );
    }
    let _ = child.wait();
}

/// Which expiry-keyed refresh should run, on a clock the tests can hold
/// still, after the session-quiet gate.
///
/// `newest` is the newest session mtime, or 0 when there are none. No
/// sessions is treated as quiet: the alternative is never refreshing a
/// signed-in machine that has not used Grok here, which is the path a
/// fresh install takes now that asking is on by default.
fn refresh_phase(
    newest: f64,
    expiry: f64,
    tried: Option<(f64, bool)>,
    at: f64,
) -> Option<bool> {
    let quiet = if newest > 0.0 { at - newest } else { f64::MAX };
    if quiet <= SESSION_QUIET {
        return None;
    }
    refresh_due(expiry, at, tried)
}

/// Refresh the token when it is about to lapse, even if no session just ended.
///
/// The session-end gate fires only in the six hours after a session ends,
/// and the token lapses on its own clock about that often. So anyone who
/// has not run Grok since yesterday — or who signed in and never has — had
/// the asking silently switched off. Reaching it needs a gate keyed on
/// the token rather than on a session.
///
/// Both of the original guards are kept: nothing happens unless asking was
/// turned on, and nothing starts the CLI underneath somebody who is using
/// it. What is dropped is the upper bound on how long ago they last did.
///
/// Deduped on the expiry value and the phase, not on time - see
/// `refresh_due`. A refresh that does not move the expiry - a login that
/// has genuinely run out - is tried once before the lapse and once after,
/// then left alone; a CLI respawned every five minutes for ever is a worse
/// failure than the stale row it was trying to fix.
fn refresh_if_token_lapsing(caches: &mut Caches, cfg: &Config, newest: f64) {
    if !cfg.grok_ping {
        return;
    }
    let Some(expiry) = token_expiry() else {
        return;
    };
    let Some(after) = refresh_phase(newest, expiry, refresh_tried(caches), now()) else {
        return;
    };
    refresh_token();
    caches.live.insert(
        EXPIRY_KEY.to_string(),
        (
            now(),
            Some(serde_json::json!({"expiry": expiry, "after": after})),
            f64::MAX,
        ),
    );
    caches.live.remove(&ping_key(token_account().as_deref()));
}

/// What the session logs add up to, kept for the passes that read
/// nothing new.
#[derive(Clone, Default)]
pub struct Built {
    sessions: usize,
    total: f64,
    daily: HashMap<NaiveDate, f64>,
}

pub fn read(caches: &mut Caches, cfg: &Config) -> Data {
    // Every session, not a window: the tab's token figure is the whole
    // history on this disk. Metadata only; contents are read below.
    let at = now();
    let (listed, complete) = list_logs(&under_home(SESSIONS), "updates.jsonl", None, at);
    let files: Vec<String> = listed.iter().map(|l| l.path.clone()).collect();
    if files.is_empty() {
        // Every session log is gone, so what was held for them goes too -
        // when the listing is complete. One cut short has not looked yet.
        if complete {
            caches.grok = Logs::default();
            caches.grok_built = None;
        }
        // The log is still read. The credit window is account-wide and true
        // whatever this disk holds, and hiding it because the local half is
        // missing is the failure this repo keeps paying for. ok stays false,
        // so the tab says there are no sessions - under the quota, not
        // instead of it.
        //
        // The expiry refresh still runs: a machine that has auth.json and
        // no session files is the fresh-install path, and returning here
        // used to skip the only refresh that can keep the ask working.
        refresh_if_token_lapsing(caches, cfg, 0.0);
        let (quota, quota_live, quota_at, quota_why) = quota_now(caches, cfg);
        return Data {
            quota,
            quota_live,
            quota_at,
            quota_why,
            quota_every: quota_every_secs(cfg),
            ..Data::default()
        };
    }
    let newest = listed.first().map_or(0.0, |l| l.mtime as f64);
    let asked = caches.asked_at > 0.0;
    // Read once and then only as it grows: a transcript that has not been
    // appended to cannot have different deltas, and re-reading every
    // session on every refresh to learn that is the whole cost of this tab.
    let pass = caches
        .grok
        .pass((listed, complete), asked, at, &mut caches.budget, |_| SessionLines::default());
    let history = caches.grok.history.clone();
    let built = match (&caches.grok_built, pass) {
        (Some(built), Pass::Held) => built.clone(),
        _ => {
            let mut built = Built::default();
            for path in &files {
                let Some(got) = caches.grok.tails.get(path.as_str()).map(|t| &t.parser) else {
                    continue;
                };
                if got.total <= 0.0 {
                    continue;
                }
                built.sessions += 1;
                built.total += got.total;
                for (day, n) in &got.days {
                    let Ok(at) = NaiveDate::parse_from_str(day, "%Y-%m-%d") else {
                        continue;
                    };
                    *built.daily.entry(at).or_insert(0.0) += n;
                }
            }
            caches.grok_built = Some(built.clone());
            built
        }
    };
    let Built { sessions, total, daily } = built;
    // A session that has just ended is the moment the numbers have changed
    // and nobody is waiting on the pane. Refreshing the token then keeps the
    // asking working; refreshing while a session is still running would mean
    // starting the CLI under somebody who is using it.
    let mut refreshed = false;
    if cfg.grok_ping && newest > 0.0 {
        let quiet = now() - newest;
        let handled = caches
            .live
            .get(SEEN_KEY)
            .and_then(|(_, v, _)| v.as_ref())
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        if (SESSION_QUIET..SESSION_STALE).contains(&quiet) && newest > handled {
            refresh_token();
            refreshed = true;
            caches.live.insert(
                SEEN_KEY.to_string(),
                (now(), Some(serde_json::json!(newest)), f64::MAX),
            );
            // The token is new, so the reading held from before it is not
            // the best one available any more.
            caches.live.remove(&ping_key(token_account().as_deref()));
        }
    }
    // One read, one spawn. The expiry helper re-reads auth.json, so a
    // session-end refresh that actually moved the token already no-ops
    // it; this skip is for the case that did not, which would otherwise
    // start the CLI a second time in the same frame. The next poll still
    // reaches the expiry gate.
    if !refreshed {
        refresh_if_token_lapsing(caches, cfg, newest);
    }
    let quota_read = quota_now(caches, cfg);
    Data {
        ok: true,
        history,
        sessions,
        files: files.len(),
        total,
        daily,
        last: newest,
        quota: quota_read.0,
        quota_live: quota_read.1,
        quota_at: quota_read.2,
        quota_why: quota_read.3,
        quota_every: quota_every_secs(cfg),
        // Decided once both vendors are read; this reader sees only Grok.
        spacexsi: Spacexsi::No,
    }
}

/// Every quota this agent publishes, for the summary screen.
///
/// One lane: the credit window is the only allowance Grok states. The
/// window length is offered only when both ends parse and run forwards,
/// but the reset is offered whenever the end does - a countdown is
/// readable without knowing how long the window was.
/// The credit window as the server has it right now.
///
/// Grok was the only agent here reading its quota off the disk, and the
/// number that produced was whatever its CLI last wrote - 23% from a window
/// that had closed, where the account had since spent 57% of the one we are
/// actually in. More than double, and nothing on screen said so.
///
/// The CLI leaves a bearer token in `auth.json` and this is the endpoint it
/// calls with it. An expired token is not sent: it would come back 401 after
/// a round trip, and the log is a better answer than a failed request. The
/// CLI refreshes the token whenever it runs, so this works for as long as
/// Grok is in use and stops when it is not, which is the honest shape.
/// The token entry the CLI left on disk, or why there is not one.
///
/// Keyed by issuer and account, so the entry is found by shape rather than
/// by a name that is different on every machine.
fn token_entry() -> Result<serde_json::Value, String> {
    let raw = std::fs::read_to_string(under_home(AUTH))
        .map_err(|_| "the Grok CLI has left no token on this disk".to_string())?;
    let parsed: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|_| "the token the CLI left is not readable JSON".to_string())?;
    parsed
        .as_object()
        .and_then(|o| {
            o.values()
                .find(|v| v.get("key").and_then(|k| k.as_str()).is_some_and(|k| !k.is_empty()))
        })
        .cloned()
        .ok_or_else(|| "the token file names no account".to_string())
}

/// The map key the CLI stored this token under, which names the issuer
/// and the account. A token refresh keeps the same key; signing in as
/// someone else does not. Pure over the file body so the tests do not
/// need a token on disk.
fn parse_token_account(raw: &str) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(raw).ok()?;
    parsed.as_object().and_then(|o| {
        o.iter()
            .find(|(_, v)| {
                v.get("key")
                    .and_then(|k| k.as_str())
                    .is_some_and(|k| !k.is_empty())
            })
            .map(|(k, _)| k.clone())
    })
}

/// Which Grok account is signed in here, for scoping what is remembered
/// about it between polls.
pub fn account_id() -> Option<String> {
    token_account()
}

fn token_account() -> Option<String> {
    parse_token_account(&std::fs::read_to_string(under_home(AUTH)).ok()?)
}

/// When the token on disk lapses, as epoch seconds.
fn token_expiry() -> Option<f64> {
    iso_epoch(&text(&token_entry().ok()?, "expires_at"))
}

/// The bearer token to ask with, or why the ask cannot even be tried.
///
/// The expiry is checked here rather than left to the server so a lapsed
/// token costs no request - but the reason is carried out instead of being
/// flattened to None, because "not live" for a token that lapsed an hour
/// ago and "not live" for an endpoint that is down are the same two words
/// for two different things, and only one of them is fixed by waiting.
fn usable_token() -> Result<String, String> {
    token_of(&token_entry()?, now())
}

/// The same decision, over a value and a clock, so it can be tested without
/// a token on disk and without waiting for one to lapse.
fn token_of(entry: &serde_json::Value, at: f64) -> Result<String, String> {
    let key = entry["key"]
        .as_str()
        .filter(|k| !k.is_empty())
        .ok_or_else(|| "the token file names no account".to_string())?
        .to_string();
    if let Some(expiry) = iso_epoch(&text(entry, "expires_at")) {
        if expiry <= at {
            return Err(format!(
                "the token lapsed {} ago - the Grok CLI refreshes it",
                left_span(at - expiry)
            ));
        }
    }
    Ok(key)
}

fn fetch_billing(key: &str, seconds: u64) -> Option<serde_json::Value> {
    get_json(
        BILLING,
        &[("Authorization", &format!("Bearer {}", key))],
        seconds,
    )
}

/// How old a reading may be and still be shown as current.
///
/// Half an hour, the same figure and the same reasoning as Claude's
/// CLAUDE_FRESH_FOR: a credit window that turns over weekly does not move
/// enough in half an hour to mislead anyone.
///
/// This tab used to mark by *where* a reading came from - `stale` was
/// simply "not from the server" - and that is the mistake claude.rs already
/// wrote down: it flags the fresher of two readings as the doubtful one.
/// Here it was worse, because the live answer was being discarded (see
/// quota_from) and the fallback was a log line eleven days old, so the row
/// said "not live" whether the ping was working or not, and turning the
/// ping on changed nothing a reader could see.
pub const GROK_FRESH_FOR: f64 = 1860.0;

/// The longest interval the poll may be set to, and the reason it and the
/// window above cannot collide.
///
/// Thirty-one minutes to call a reading old, thirty as the most anyone can
/// wait between asks: the ceiling sits a minute under the threshold, so the
/// freshest answer the widget can hold is always inside the window that
/// judges it. Before this the two were unrelated, and an interval of sixty
/// made the widget mark its own newest reading as doubtful for the back
/// half of every cycle - a warning about something no setting of the
/// reader's could fix.
pub const GROK_PING_MAX: f64 = 1800.0;

/// How long a reading is held, from the interval the reader asked for.
///
/// Clamped rather than refused: a config file is hand-edited as often as it
/// is set through the settings screen, and a number quietly doing the
/// nearest sane thing beats a widget that will not start. The tab says the
/// interval it actually used, so a clamped value is visible rather than
/// silent.
///
/// Its own function so the clamp under test is the one the widget runs. The
/// first version of its test rewrote the clamp beside it and passed with
/// the production one deleted - the shape that makes a guard look held and
/// leaves it loose.
pub fn ping_ttl(minutes: f64) -> f64 {
    (minutes * 60.0).clamp(60.0, GROK_PING_MAX)
}

/// The interval the tab prints, which is the one the poll uses.
///
/// A hand-edited file can name sixty; the poll still waits thirty, and
/// this is what makes that visible. Both construction sites used to
/// multiply the raw minutes, so a clamp on the poll left the tab saying
/// every 1h. Its own function so the number under test is the one the
/// tab is given, not a copy of it.
pub fn quota_every_secs(cfg: &Config) -> f64 {
    cfg.grok_ping.then(|| ping_ttl(cfg.grok_ping_minutes)).unwrap_or(0.0)
}

/// True when the reading is old enough to be worth flagging, whatever its
/// source. A reading with no timestamp at all is treated as old, because
/// unknown age is not evidence of youth.
fn reading_is_old(taken: Option<f64>) -> bool {
    taken.is_none_or(|t| now() - t > GROK_FRESH_FOR)
}

/// A dollar figure as x.ai states it: whole where it is whole.
///
/// These are dollars, not cents - `onDemandCap {val: 25}` is $25 - and
/// "$25.00" spends two cells saying the same thing as "$25".
fn dollars(v: f64) -> String {
    if (v - v.round()).abs() < 0.005 {
        format!("${}", v.round() as i64)
    } else {
        format!("${:.2}", v)
    }
}

/// The paid allowance as a percentage, with the cap it is a percentage of -
/// or nothing, where the account has set no cap.
///
/// Nothing is the account's state rather than a zero, and the difference
/// matters: a 0% bar would say there is an on-demand allowance sitting
/// untouched, when what is true is that there is none. The same refusal
/// `sand_lane` makes for a Bot allowance nobody granted, and Cursor's
/// extra usage for a spend limit nobody set.
///
/// Spend with no cap beside it is proto3 omitting the default, which on
/// this endpoint means nought - the same reading the credit percentage
/// gets three screens up, for the same documented reason.
///
/// Not clamped: x.ai has not been seen to report spend past the cap, and
/// if it ever does, the real figure is the one worth drawing.
fn on_demand_lane(q: &Quota) -> Option<(f64, f64)> {
    let cap = q.on_demand_cap.filter(|c| *c > 0.0)?;
    Some((100.0 * q.on_demand_used.unwrap_or(0.0) / cap, cap))
}

/// The week split by product, in the order the server lists them.
///
/// A product with nothing spent omits `usagePercent` exactly as the total
/// does, and for the same reason, so an absent one reads as nought here
/// too. Products are kept even at nought: which of the three is idle is
/// part of the answer, and dropping them would leave a reader unable to
/// tell an unused product from one the server stopped reporting.
fn products_of(cfg: &serde_json::Value) -> Vec<(String, f64)> {
    cfg["productUsage"]
        .as_array()
        .map(|list| {
            list.iter()
                .map(|entry| {
                    // The unified plan's answer carries an entry with no
                    // product named - 3% of a 5% week on the measured
                    // account, beside GrokBot's 2%. Dropping it left a split
                    // that read as the whole week and was not, so it is kept
                    // and called what it is rather than given a name the
                    // server did not send.
                    // Kept as the server sent it - empty included - and named
                    // for display by `product_name`, which knows which plan
                    // the reading came from.
                    let name = text(entry, "product");
                    (name, val_of(&entry["usagePercent"]).unwrap_or(0.0))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The prepaid balance in dollars.
///
/// x.ai sends it in cents, unlike the on-demand cap beside it, which is
/// dollars. Measured on the one account that has ever held a balance here:
/// `prepaidBalance {val: 2000}` for the $20 paid on cursor.com, the same
/// balance Cursor's own GetUserPrepaidBilling states as `availableCents:
/// "2000"`. Read as dollars it said `prepaid $2000`, a hundredfold error in
/// a figure about money.
fn prepaid_of(cfg: &serde_json::Value) -> Option<f64> {
    val_of(&cfg["prepaidBalance"]["val"]).map(|cents| cents / 100.0)
}

/// Whether the answer carries a marker only the unified Cursor and Grok
/// plan sends.
///
/// Not `isUnifiedBillingUser`: the log carries that as true on readings
/// from before the plans merged, on a Free tier with its own window, so it
/// says something about x.ai's billing and nothing about Cursor. What
/// arrived with the merge, on the credits endpoint, is a top-up method for
/// the shared prepaid balance and a `GrokBot` product in the week's split -
/// Grok Bot being the allowance Cursor bills. A log line has neither, which
/// is why the window match in `vendors::link_spacexsi` exists as the
/// second route in.
fn unified_markers(cfg: &serde_json::Value) -> bool {
    !text(cfg, "topUpMethod").is_empty()
        || cfg["productUsage"]
            .as_array()
            .is_some_and(|list| list.iter().any(|e| text(e, "product") == "GrokBot"))
}

/// Whether the reading on hand is the server's, and recent: asked just now
/// rather than read back from the log, not older than the window that
/// judges a reading current, and of a week that has not yet ended - a
/// cached answer can outlive its reset, and its split is last week's.
pub fn reading_is_current(d: &Data) -> bool {
    d.quota_live
        && d.quota.as_ref().is_some_and(|q| {
            !reading_is_old(q.taken) && iso_epoch(&q.end).is_none_or(|end| end > now())
        })
}

/// Whether this reading says the account is on the unified plan.
pub fn unified(d: &Data) -> bool {
    d.quota.as_ref().is_some_and(|q| q.unified)
}

/// The credit window, as (start, end) in epoch milliseconds - the
/// fingerprint `vendors::link_spacexsi` matches against Cursor's pool.
///
/// A reading may describe a week that has since rolled over - a log line,
/// or a live answer still held in the poll cache across the reset - so a
/// window that has ended is projected onto the current week the way the
/// lanes project it, or the same account's two windows could never match
/// after a reset. A window still running is taken as sent.
pub fn pool_window(d: &Data) -> Option<(f64, f64)> {
    pool_window_at(d, now())
}

/// `pool_window` as it reads at `at`, so a test can fix the clock: a
/// window that has closed by then is rolled forward, and one that has not
/// is taken as it is.
fn pool_window_at(d: &Data, at: f64) -> Option<(f64, f64)> {
    let q = d.quota.as_ref()?;
    let (s, e) = match (iso_epoch(&q.start), iso_epoch(&q.end)) {
        (Some(s), Some(e)) if e > s => (s, e),
        _ => return None,
    };
    match window_now(Some(s), Some(e), at) {
        (Some(end), true) => Some(((end - (e - s)) * 1000.0, end * 1000.0)),
        _ => Some((s * 1000.0, e * 1000.0)),
    }
}

/// The week split by product, one `name ~pct` entry after another, each
/// entry whole: an entry that does not fit carries on to the next row,
/// under the first, rather than be cut and lose its percentage.
fn by_product_rows(entries: Vec<String>, w: usize, p: &Palette) -> Vec<String> {
    // Later rows sit under the first entry, or at a short indent where an
    // entry would not fit even there.
    let head = "  by product ";
    let under = " ".repeat(head.chars().count());
    let short = "    ";
    let room = |lead: &str| w.saturating_sub(1).saturating_sub(lead.chars().count()).max(1);
    let mut lines: Vec<(String, String)> = vec![(head.to_string(), String::new())];
    for entry in entries {
        let (lead, last) = lines.last_mut().unwrap();
        let joined = if last.is_empty() { entry.clone() } else { format!("{last} · {entry}") };
        if last.is_empty() || joined.chars().count() <= room(lead) {
            *last = joined;
        } else {
            let lead = if entry.chars().count() <= room(&under) { under.clone() } else { short.to_string() };
            lines.push((lead, entry));
        }
    }
    lines
        .into_iter()
        .map(|(lead, text)| tc::seg(&[(p.dim.as_str(), lead), (p.txt.as_str(), text)], w - 1))
        .collect()
}

/// A product as the split names it on screen.
///
/// On the unified plan the entry the server leaves unnamed is Cursor. Not a
/// guess at a default: matched against x.ai's own usage dashboard on
/// 2026-10-13, which listed Cursor 134,394,024 tokens, Grok Bot 137,782,665,
/// Grok Chat 2,384,739 and Grok Build 103,749 - shares of 48.9%, 50.2%,
/// 0.87% and 0.04% of a 10.3% week, so 5.03%, 5.16%, 0.09% and 0.004%.
/// The endpoint answered `[{usagePercent: 5.0}, {product: "GrokBot",
/// usagePercent: 5.0}]`: whole percents, and a product that rounds to
/// nought absent altogether. Grok Build carries a name of its own on the
/// account's own plan, so the unnamed one is not Build; its share is
/// Cursor's. The other names are spaced the way the dashboard spells them.
///
/// On the account's own plan the names stay exactly as they were drawn
/// before, and an unnamed entry is called what it is.
fn product_name(raw: &str, unified: bool) -> String {
    if !unified {
        return if raw.is_empty() { "unnamed".to_string() } else { raw.to_string() };
    }
    if raw.is_empty() {
        return "Cursor".to_string();
    }
    let mut out = String::new();
    for (i, c) in raw.chars().enumerate() {
        if i > 0 && c.is_ascii_uppercase() {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

/// The week's pool split by product, for the SpaceXSI section: display
/// name and whole percent, since that is all the endpoint sends, in the
/// server's order.
pub fn pool_split(d: &Data) -> Vec<(String, f64)> {
    let Some(q) = d.quota.as_ref() else {
        return Vec::new();
    };
    let unified = d.spacexsi.unified() || q.unified;
    q.products.iter().map(|(name, pct)| (product_name(name, unified), *pct)).collect()
}

/// The billing body in the same shape the log parser produces, so the rest
/// of the tab cannot tell which of the two it is looking at.
fn quota_from(d: &serde_json::Value) -> Option<Quota> {
    let cfg = &d["config"];
    let period = &cfg["currentPeriod"];
    // The period is what makes this a reading; the percentage is optional.
    //
    // It used to be the other way round - no percentage, no reading - and
    // that has stopped being true of the endpoint. x.ai no longer sends
    // `creditUsagePercent` for unified-billing accounts: both `/v1/billing`
    // and `?format=credits` answer 200, name the current weekly period, and
    // omit it entirely. Refusing the whole answer for that meant falling
    // back to the newest log line, which on this machine was written eleven
    // days ago about a window that closed a week before that - a fossil
    // shown as current while the server's own answer, naming the window we
    // are actually in, was thrown away.
    //
    // The log parser accepts a reading whose percentage is absent, for
    // exactly this reason. The two are consistent now.
    let start = text(period, "start");
    let end = text(period, "end");
    if start.is_empty() || end.is_empty() {
        return None;
    }
    Some(Quota {
        // An absent percentage against a named period means nought, not
        // unknown. This is proto3 omitting a scalar at its default, and it
        // was mistaken here for the field having been withdrawn.
        //
        // The evidence is inside a single response. Alongside the credit
        // figure the endpoint returns `productUsage`, one entry per product,
        // and on this account it reads:
        //
        //     [{"product":"GrokBuild","usagePercent":1.0},
        //      {"product":"GrokChat"},{"product":"GrokImagine"}]
        //
        // The product with usage carries the key; the two at zero omit it,
        // in the same array, in the same answer. Watched over time as well:
        // this account's weekly window reset at 02:09, read with no
        // percentage at all while nothing had been spent, and began
        // reporting 1.0 once it had - same endpoint, same headers, same
        // token.
        //
        // So nought is the real reading and not a guess, which is the only
        // reason it may be drawn - and only while nothing in the same answer
        // says otherwise. An omitted total beside a product that reports
        // use is not nought: it is a total the answer did not give, and
        // drawing 0% there invents a reading against the evidence in the
        // same response. That is unknown, the same as a total never sent.
        // A period this cannot parse is still unknown, and still refused
        // above.
        pct: match val_of(&cfg["creditUsagePercent"]) {
            Some(v) => Some(v),
            None if products_of(cfg).iter().any(|(_, used)| *used > 0.0) => None,
            None => Some(0.0),
        },
        products: products_of(cfg),
        taken: Some(now()),
        kind: text(period, "type"),
        start,
        end,
        tier: String::new(),
        on_demand_used: val_of(&cfg["onDemandUsed"]["val"]),
        on_demand_cap: val_of(&cfg["onDemandCap"]["val"]),
        prepaid: prepaid_of(cfg),
        unified: unified_markers(cfg),
    })
}

/// The window we are in now, rolled forward from the one the log recorded.
///
/// Grok publishes no live quota: the reading is whatever its own CLI last
/// wrote to its log, and that can be weeks old. Once the recorded window has
/// ended, its end date is not a reset to count down to - it is a date that
/// has been and gone, which is why this row read "resetting" for ever
/// instead of counting down to anything.
///
/// The window rolls forward on its own measured length rather than on an
/// assumed seven days: the server states the period, and a window that
/// turns out to be fortnightly should not be guessed weekly. That makes the
/// answer a calculation rather than an observation, so it is flagged and
/// the screen prints it with a `~`.
///
/// Returns the recorded end unchanged when there is nothing to roll: no
/// length to roll by, or a window that has not ended yet.
fn window_now(begin: Option<f64>, end: Option<f64>, at: f64) -> (Option<f64>, bool) {
    let (Some(b), Some(e)) = (begin, end) else {
        return (end, false);
    };
    let len = e - b;
    if len <= 0.0 || e > at {
        return (end, false);
    }
    let skipped = ((at - e) / len).floor() + 1.0;
    (Some(e + skipped * len), true)
}

pub fn lanes(d: &Data) -> Vec<Lane> {
    let Some(q) = d.quota.as_ref() else {
        return Vec::new();
    };
    let (begin, end) = (iso_epoch(&q.start), iso_epoch(&q.end));
    // A live reading is of the window we are in, so there is nothing to roll
    // forward and nothing to qualify - until that window ends while the
    // answer is still held in the poll cache, when it is last week's figure
    // as much as a log line is, and is rolled and qualified the same way.
    let (reset, projected) = if d.quota_live && end.is_none_or(|e| e > now()) {
        (end, false)
    } else {
        window_now(begin, end, now())
    };
    let window_secs = match (begin, end) {
        (Some(b), Some(e)) if e > b => Some(e - b),
        _ => None,
    };
    // By age, not by source. A figure the server sent four minutes ago
    // and one the log recorded thirty seconds ago are both current; a
    // live fetch of a reading taken days earlier is not.
    let stale = reading_is_old(q.taken);
    let mut out = Vec::new();
    // The credit percentage is optional and the on-demand cap is optional,
    // and they are optional independently: a unified-billing account gets
    // no `creditUsagePercent` at all, which used to take the whole of Grok
    // off the summary and would now take the paid allowance with it.
    if let Some(pct) = q.pct {
        out.push(Lane {
            // On the unified plan this is the weekly pool Cursor draws on
            // too, and naming it as Cursor's tab names it is what lets the
            // two be read as one allowance.
            label: if d.spacexsi.unified() { crate::cursor::POOL_LABEL } else { "credits" }.into(),
            pct,
            window_secs,
            reset,
            stale,
            projected,
            apart: false,
        });
    }
    // The allowance that costs money, on the same window as the credits
    // beside it so the two pace against one clock. Its label carries the
    // cap: a percentage of an unnamed ceiling is not a number anyone can
    // act on, and this one is dollars.
    if let Some((pct, cap)) = on_demand_lane(q) {
        out.push(Lane {
            label: format!("on-demand{}", cap_tag(cap)),
            pct,
            window_secs,
            reset,
            // The same reading, so the same age.
            stale,
            projected,
            apart: false,
        });
    }
    out
}

/// True when nothing is asking the server on the reader's behalf, so the
/// figures move only when they use Grok on this machine. The summary says so
/// under the row; once asking is on, the tab reports the interval instead.
pub fn asks_nobody(d: &Data) -> bool {
    d.quota_every <= 0.0
}

/// Why Grok publishes no bar, when it does not.
///
/// "No quota published" covers two situations that want opposite things
/// from the reader. Nobody is asking, and they could turn asking on - or
/// the ask is working and x.ai is the one with nothing to report, in which
/// case there is nothing for them to do and a prompt to change a setting
/// would be a wild goose chase.
pub fn why_no_lane(d: &Data) -> String {
    if !lanes(d).is_empty() {
        return String::new();
    }
    if !d.quota_why.is_empty() {
        return format!("no quota · {}", d.quota_why);
    }
    if d.quota.is_some() && d.quota_live {
        return "no quota · x.ai answered, and published no credit figure for this period. \
                Accounts on unified billing stopped carrying one; the window it does \
                state is on the GROK tab."
            .into();
    }
    if d.quota_every <= 0.0 {
        return tc::missing_config(
            "no quota · asking x.ai is off (set agent_usage.grok_ping to poll) and \
             ~/.grok/logs/unified.jsonl has no creditUsagePercent.",
        );
    }
    "no quota · x.ai did not publish a credit figure, and the log has none either.".into()
}

/// Where the figure came from, in the two states it can be in.
///
/// Grok is the only agent here that can be reading a file rather than a
/// server, and the file goes stale in silence: the last time it did it
/// showed 23% of a window that had closed, while the account had spent 57%
/// of the one it was in. A percentage nobody dated reads as current.
///
/// Written for someone who will go and look: it names the file, its age and
/// the settings, rather than explaining what a stale reading is.
/// An interval as a reader would say it: "1h", not left_span's "1h 0m",
/// which is a duration formatter answering a question nobody asked.
fn every(seconds: f64) -> String {
    let mins = (seconds.max(60.0) / 60.0).round() as u64;
    match (mins / 60, mins % 60) {
        (0, m) => format!("{}m", m),
        (h, 0) => format!("{}h", h),
        (h, m) => format!("{}h {}m", h, m),
    }
}

fn freshness(d: &Data, w: usize, p: &Palette) -> Vec<String> {
    let mut out = Vec::new();
    if d.quota_every > 0.0 {
        let fresh = !reading_is_old(d.quota.as_ref().and_then(|q| q.taken));
        let ago = now() - d.quota_at;
        let last = if d.quota_at <= 0.0 {
            "not yet".to_string()
        } else if ago < 90.0 {
            "just now".to_string()
        } else {
            format!("{} ago", left_span(ago))
        };
        out.push(tc::seg(
            &[
                // Whether the figure is current, which is a question about
                // its age. It used to be a question about its source, so a
                // working ping still read "not live" for as long as the
                // answer it fetched was being discarded.
                (
                    if fresh { p.ok.as_str() } else { p.warn.as_str() },
                    if fresh { "  live" } else { "  not live" }.to_string(),
                ),
                (
                    p.dim.as_str(),
                    format!(" · polled x.ai {}, every {}", last, every(d.quota_every)),
                ),
                // "not live" alone is one phrase for four situations, and
                // three of them are fixable by the reader. Saying which
                // costs a clause and is the difference between a widget
                // that looks broken and one that says what to do.
                (
                    p.dim.as_str(),
                    if d.quota_why.is_empty() {
                        String::new()
                    } else {
                        format!(" · {}", d.quota_why)
                    },
                ),
            ],
            w - 1,
        ));
        out.push(String::new());
        return out;
    }
    // The reading's age, not the file's. The CLI touches that log whenever
    // it starts, so a file written minutes ago can still hold a credit
    // figure from a fortnight back - and "written 17m ago" beside a
    // percentage reads as a fresh percentage.
    let closed = d
        .quota
        .as_ref()
        .and_then(|q| iso_epoch(&q.end))
        .filter(|e| *e <= now())
        .map(|e| format!(" · window closed {} ago", left_span(now() - e)))
        .unwrap_or_default();
    out.push(tc::seg(
        &[
            (p.warn.as_str(), "  not live".into()),
            (p.dim.as_str(), format!(" · ~/{}{}", LOG, closed)),
        ],
        w - 1,
    ));
    out.extend(
        tc::wrap_words(
            &tc::missing_config(
                "Only your own Grok sessions update it. agent_usage.grok_ping polls x.ai instead.",
            ),
            w.saturating_sub(3).max(1),
        )
        .into_iter()
        .map(|line| {
            tc::seg(
                &[(p.dim.as_str(), format!("  {line}"))],
                w.saturating_sub(1).max(1),
            )
        }),
    );
    out.push(String::new());
    out
}

/// What the server calls this window, in words a reader has met before.
fn period_name(kind: &str) -> String {
    if kind.contains("WEEKLY") {
        return "weekly".into();
    }
    match kind.replace("USAGE_PERIOD_TYPE_", "").to_lowercase() {
        s if s.is_empty() => "current".into(),
        s => s,
    }
}

/// A date without its year, kept in the offset it arrived in.
///
/// iso_day is the widget's parser for these and deliberately does not
/// convert to this machine's zone: a billing window that reads a day
/// earlier here than on the vendor's own page is worse than no window at
/// all. The year goes because both ends of a window share it.
fn short_day(s: &str) -> String {
    let full = iso_day(s);
    match full.rsplit_once(' ') {
        Some((day, _year)) => day.to_string(),
        None => full,
    }
}

/// A row from owned pairs, which is what paced_bar and the calendar hand
/// back - seg borrows its colours.
fn seg_of(parts: &[(String, String)], w: usize) -> String {
    let refs: Vec<(&str, String)> = parts.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
    tc::seg(&refs, w - 1)
}

fn grok_tab_with_plots(d: &Data, w: usize, p: &Palette, plots: &mut Vec<tc::Plot>) -> Vec<String> {
    let hue = agent_hue("grok");
    let mut rows: Vec<String> = Vec::new();
    // Not filtered on the percentage any more. A reading that names the
    // window but not the spend is still a reading, and hiding the whole
    // section for it is the failure this repo keeps paying for: the pane
    // goes blank and a reader cannot tell an account with no quota from a
    // widget that has stopped working. The window, the countdown and the
    // reason are all still true; only the bar needs a number.
    if let Some(q) = d.quota.as_ref() {
        // The one real remaining-quota figure in this widget: everything
        // else here counts what was spent. It leads the tab for that
        // reason.
        let (begin, end) = (iso_epoch(&q.start), iso_epoch(&q.end));
        // The window we are in, not the one the reading came from - the
        // heading used the raw end, so a rolled window lost its countdown
        // here while the summary still had one. Two screens, one answer.
        // A live answer still held after its week ended is rolled too, as
        // the summary rolls it.
        let (current, rolled) = if d.quota_live && end.is_none_or(|e| e > now()) {
            (end, false)
        } else {
            window_now(begin, end, now())
        };
        let left = current.map(|e| e - now()).filter(|secs| *secs >= 0.0);
        // On the unified plan the window is the pool Cursor's tab draws, so
        // it goes under the same heading. Its own plan keeps the heading it
        // always had.
        let unified_here = d.spacexsi.unified();
        let heading = if unified_here {
            format!(" ── {} ── ", SPACEXSI)
        } else {
            format!(" ── {} QUOTA ── ", period_name(&q.kind).to_uppercase())
        };
        let via = match d.spacexsi {
            Spacexsi::Separate => format!("via Grok · {} · ", period_name(&q.kind)),
            Spacexsi::No => String::new(),
            _ => format!("{} · ", period_name(&q.kind)),
        };
        rows.push(tc::seg(
            &[
                (p.lbl.as_str(), heading),
                (p.dim.as_str(), via),
                (
                    p.dim.as_str(),
                    // left_span, not a decimal of a day: "~0.9 days" is
                    // most of a day away stated in the least useful unit,
                    // and the reader wanting to know if they can finish
                    // something before the reset needs hours and minutes.
                    left.map(|secs| {
                        format!("resets in {}{}", if rolled { "~" } else { "" }, left_span(secs))
                    })
                    .unwrap_or_default(),
                ),
            ],
            w - 1,
        ));
        rows.extend(freshness(d, w, p));
        // A window is only a window if it runs forwards; without both ends
        // there is no pace to report, and a mark placed anyway would be a
        // claim about a clock nobody read.
        let (span, reset) = match (begin, end) {
            (Some(b), Some(e)) if e > b => (Some(e - b), Some(e)),
            _ => (None, None),
        };
        // The bar is the one part that needs a number. Without one the row
        // says so in words rather than drawing an empty gauge, which would
        // read as nought per cent used.
        match q.pct {
            Some(pct) => {
                let mut line: Vec<(String, String)> = vec![(
                    pct_colour(pct, hue, p),
                    format!(" {:<5}", format!("{:.0}%", pct)),
                )];
                line.extend(paced_bar_with_plot(plots, rows.len(), &line,
                    (pct / 100.0).clamp(0.0, 1.0),
                    elapsed_of(span, reset),
                    w.saturating_sub(38).max(10),
                    hue,
                    p,
                ));
                line.push((
                    p.dim.clone(),
                    if unified_here { "  of the weekly pool" } else { "  credits used" }.into(),
                ));
                line.push(pace_cell(
                    lead(pct, span, reset),
                    pace_guessed(reading_is_old(q.taken), rolled, reset),
                    p,
                ));
                rows.push(seg_of(&line, w));
            }
            None => rows.push(tc::seg(
                &[(p.dim.as_str(), "  no credit figure for this period".into())],
                w - 1,
            )),
        }
        // On-demand gets the credits row's treatment rather than a tail on
        // the window line, so the tab and the summary say the same thing
        // about the allowance that is billed. Without a cap it says so in
        // words: an empty gauge here would read as an untouched allowance,
        // and there is not one.
        match on_demand_lane(q) {
            Some((pct, cap)) => {
                let tail = format!(
                    "  on-demand {} of {}",
                    dollars(q.on_demand_used.unwrap_or(0.0)),
                    dollars(cap)
                );
                let mut line: Vec<(String, String)> = vec![(
                    pct_colour(pct, hue, p),
                    format!(" {:<5}", format!("{:.0}%", pct)),
                )];
                line.extend(paced_bar_with_plot(plots, rows.len(), &line,
                    (pct / 100.0).clamp(0.0, 1.0),
                    elapsed_of(span, reset),
                    // Measured against the tail actually being drawn, so a
                    // longer cap cannot quietly start clipping the row.
                    w.saturating_sub(15 + tail.chars().count()).max(8),
                    hue,
                    p,
                ));
                line.push((p.dim.clone(), tail));
                line.push(pace_cell(
                    lead(pct, span, reset),
                    pace_guessed(reading_is_old(q.taken), rolled, reset),
                    p,
                ));
                rows.push(seg_of(&line, w));
            }
            // The unified plan has no on-demand cap of x.ai's own: extra use
            // bills to the shared prepaid balance, under a monthly limit set
            // on cursor.com. "No cap set" would say there is no limit when
            // there may well be one, so the row says where the money is.
            None if d.spacexsi == Spacexsi::Shared => rows.push(tc::seg(
                &[(
                    p.dim.as_str(),
                    "  same account as the CURSOR tab - credits and the monthly limit are there"
                        .into(),
                )],
                w - 1,
            )),
            None if unified_here => rows.push(tc::seg(
                &[(
                    p.dim.as_str(),
                    "  extra use bills to prepaid credits, under a limit set on cursor.com".into(),
                )],
                w - 1,
            )),
            None => rows.push(tc::seg(
                &[(p.dim.as_str(), "  no on-demand cap set".into())],
                w - 1,
            )),
        }

        let (from, to) = (short_day(&q.start), short_day(&q.end));
        let window = if from.is_empty() || to.is_empty() {
            "?".to_string()
        } else {
            format!("{} → {}", from, to)
        };
        // On-demand has a row of its own above; what is left here is the
        // prepaid balance, which is money on the account rather than an
        // allowance and so has no percentage to be a bar of.
        let mut extras: Vec<String> = Vec::new();
        // Shared, the balance is Cursor's tab's to show: it is one balance,
        // and stating it on both tabs reads as two.
        if let Some(prepaid) = q.prepaid.filter(|v| *v != 0.0) {
            if d.spacexsi != Spacexsi::Shared {
                extras.push(format!("prepaid {}", dollars(prepaid)));
            }
        }
        rows.push(tc::seg(
            &[
                (p.dim.as_str(), "  window ".into()),
                (p.txt.as_str(), window),
                (
                    p.dim.as_str(),
                    if extras.is_empty() {
                        String::new()
                    } else {
                        format!("   {}", extras.join(" · "))
                    },
                ),
            ],
            w - 1,
        ));
        // The same week split by product. Worth a row of its own because
        // the bar above is one number for three different things, and which
        // of them is spending is the part a reader can act on.
        if unified_here {
            // The unified plan's split is whole percents with Cursor's share
            // unnamed; named and said for what it is.
            let split = pool_split(d);
            if !split.is_empty() {
                let entries = split.iter().map(|(name, pct)| format!("{} ~{:.0}%", name, pct)).collect();
                rows.extend(by_product_rows(entries, w, p));
            }
        } else if !q.products.is_empty() {
            let entries = q
                .products
                .iter()
                .map(|(name, pct)| format!("{} {}", product_name(name, false), pct_text(*pct).trim()))
                .collect();
            rows.extend(by_product_rows(entries, w, p));
        }
        rows.push(String::new());
    } else {
        let note = why_no_lane(d);
        if !note.is_empty() {
            rows.extend(no_local(&note, "", w, p));
            rows.push(String::new());
        }
    }

    // SpaceXSI is drawn from the account's own figures only: the session
    // totals below are this machine's transcripts, one machine's share of
    // an account-wide plan.
    if d.spacexsi.unified() {
        return rows;
    }
    // Everything below counts what this machine recorded, so it stops here
    // when there is nothing recorded - but the quota above has already been
    // drawn, because it is a fact about the account rather than the disk.
    if !d.ok {
        rows.extend(no_local(
            "No Grok sessions on this machine.",
            run_hint("grok"),
            w,
            p,
        ));
        return rows;
    }

    rows.extend(crate::history_rows(&d.history, "session logs", w, p));
    rows.push(tc::seg(
        &[
            (
                p.lbl.as_str(),
                if d.history.partial() { " ── SO FAR ── " } else { " ── TOTALS ── " }.into(),
            ),
            (
                p.dim.as_str(),
                format!("{} sessions · newest {} ago", d.sessions, ago(d.last)),
            ),
        ],
        w - 1,
    ));
    rows.push(tc::seg(
        &[
            (p.dim.as_str(), "  tokens ".into()),
            (p.agent.as_str(), big_num(d.total)),
            (
                p.dim.as_str(),
                format!("   across {} session files", d.files),
            ),
        ],
        w - 1,
    ));

    if let Some(cal) = day_calendar(&d.daily, w, GROK_STEPS, None, p) {
        let peak = d.daily.values().cloned().fold(0.0f64, f64::max);
        rows.push(String::new());
        rows.push(tc::seg(
            &[
                (p.lbl.as_str(), " ── TOKENS / DAY ── ".into()),
                (p.dim.as_str(), "peak ".into()),
                (p.agent.as_str(), big_num(peak)),
                (
                    p.dim.as_str(),
                    format!(
                        " on {}",
                        cal.best
                            .map(|b| format!("{} {}", MONTHS[b.month0() as usize], b.day()))
                            .unwrap_or_else(|| "--".into())
                    ),
                ),
            ],
            w - 1,
        ));
        let mut plot = cal.plot.clone();
        plot.y += rows.len();
        plots.push(plot);
        for line in &cal.rows {
            rows.push(seg_of(line, w));
        }
        let mut legend: Vec<(String, String)> = vec![(p.dim.clone(), "  Less ".into())];
        legend.extend(
            GROK_STEPS
                .iter()
                .map(|(r, g, b)| (tc::rgb(*r, *g, *b), "█".to_string())),
        );
        legend.push((p.dim.clone(), " More".into()));
        rows.push(seg_of(&legend, w));
    }

    rows.push(String::new());
    // Where the totals come from, because a running counter summed as
    // deltas is not what a reader assumes a token total is. The second
    // sentence points at something on screen, so it is only written when
    // that something is there.
    let mut note = "Totals are a running count per session, summed as deltas so a session \
                    spanning days lands on the right one."
        .to_string();
    // Either way it is the server's own figure and not an inference - but
    // which of the two routes it took is a different sentence, and the note
    // named only the log for as long as the log was the only route that
    // ever produced one.
    if d.quota.is_some() {
        note.push_str(if d.quota_live {
            " The quota above is the server's own figure, asked for directly - not inferred."
        } else {
            " The quota above is the server's own figure, read from the client log - not \
             inferred."
        });
    }
    for line in wrap_text(&note, w.saturating_sub(4).max(20)) {
        rows.push(tc::seg(&[(p.dim.as_str(), format!("  {}", line))], w - 1));
    }
    rows
}

/// Grok states its tier and the kind of period it bills in, and no more.
///
/// Both arrive on the client log beside the credit percentage, so this
/// costs nothing extra to show.
fn plan_block(d: &Data, w: usize, p: &Palette) -> Vec<String> {
    let Some(q) = d.quota.as_ref() else {
        return Vec::new();
    };
    let mut pairs: Vec<(String, String)> = Vec::new();
    let kind = q.kind.replace("USAGE_PERIOD_TYPE_", "").to_lowercase();
    if !kind.is_empty() {
        pairs.push(("billing period".into(), kind));
    }
    // A cap of nought is not a cap, and this row said so as "0 of 0 used" -
    // an allowance fully unspent, which is the opposite reading. The
    // quota block above draws no bar for the same reason and says which
    // state it is in; this says the same thing in the same words.
    if let Some(cap) = q.on_demand_cap {
        pairs.push((
            "on-demand".into(),
            if cap > 0.0 {
                format!(
                    "{} of {} used",
                    dollars(q.on_demand_used.unwrap_or(0.0)),
                    dollars(cap)
                )
            } else {
                "no cap set".into()
            },
        ));
    }
    // Shared, the balance is the CURSOR tab's to state, as on the quota
    // block above.
    if let Some(prepaid) = q.prepaid.filter(|_| d.spacexsi != Spacexsi::Shared) {
        pairs.push(("prepaid balance".into(), dollars(prepaid)));
    }
    let note = if d.spacexsi.unified() { SPACEXSI } else { "" };
    plan_rows(&q.tier, &pairs, w, note, None, "", p)
}

/// The whole tab: the credit window, what the transcripts recorded, and
/// which subscription that percentage is a percentage of.
///
/// No METERED section, unlike the other tabs. Grok's events carry one
/// running total with no model on it and no split by priced kind, and
/// input, output and the two cache durations differ in price by up to
/// fifty times - so a total here cannot be costed at all.
pub fn tab_with_plots(d: &Data, w: usize, _h: usize, _cfg: &Config, p: &Palette, plots: &mut Vec<tc::Plot>) -> Vec<String> {
    add_section(grok_tab_with_plots(d, w, p, plots), plan_block(d, w, p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_read_as_it_grows_matches_one_read_whole() {
        let mut lines = Vec::new();
        for i in 0..40u64 {
            // The counter steps back once, as a resumed session's replay does.
            let total = if i == 20 { 500 } else { 1000 * (i + 1) };
            lines.push(format!(
                r#"{{"totalTokens":{total},"agentTimestampMs":{},"note":"ünï ✓"}}"#,
                1_788_000_000_000u64 + i * 3_600_000
            ));
        }
        let body = lines.join("\n") + "\n";
        let replacement = "{\"totalTokens\":7,\"agentTimestampMs\":1788000000000}\n";
        let sorted = |(total, days): (f64, HashMap<String, f64>)| {
            let mut days: Vec<(String, f64)> = days.into_iter().collect();
            days.sort_by(|a, b| a.0.cmp(&b.0));
            (total, days)
        };
        for step in [1, 17, 300] {
            growing::check(
                "grok",
                &body,
                step,
                replacement,
                SessionLines::default,
                |text| sorted(session_days(text)),
                |p: &SessionLines| sorted((p.total, p.days.clone())),
            );
        }
    }

    /// A client-log line carrying a credit reading. Invented, but shaped
    /// like the real thing down to the microseconds and the numeric offset
    /// on the period - a fixture that arrives in a tidier format than the
    /// server sends tests a parser nobody has.
    pub(super) const LOG_LINE: &str = concat!(
        r#"{"msg":"config","lvl":"info","ctx":{"subscriptionTier":"Test Premium","config":{"#,
        r#""creditUsagePercent":42.5,"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY","#,
        r#""start":"2026-08-10T00:00:00.000000+00:00","#,
        r#""end":"2026-08-17T00:00:00.000000+00:00"},"#,
        r#""onDemandUsed":{"val":"3"},"onDemandCap":{"val":25},"#,
        r#""prepaidBalance":{"val":0}}}}"#
    );

    /// A drawn row without its colour escapes, so a width is cells on
    /// screen rather than bytes in the string.
    fn visible(row: &str) -> String {
        let mut out = String::new();
        let mut chars = row.chars();
        while let Some(c) = chars.next() {
            if c == '\u{1b}' {
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    /// The same log line from an account that has set no on-demand cap,
    /// for the tests whose subject is the credit lane alone.
    fn no_cap_line() -> String {
        LOG_LINE.replace(r#""onDemandCap":{"val":25}"#, r#""onDemandCap":{"val":0}"#)
    }

    #[test]
    fn a_clamped_interval_does_not_print_as_an_hour() {
        // Sixty in the file is thirty on the glass. `every` is what the
        // tab runs, so a copy of the clamp that never reached it would
        // still say "1h".
        let cfg = Config {
            grok_ping: true,
            grok_ping_minutes: 60.0,
            ..Config::default()
        };
        assert_eq!(every(quota_every_secs(&cfg)), "30m");
    }

    #[test]
    fn the_quota_comes_off_the_log_not_the_transcript() {
        // A session event carries a running token count and says nothing
        // about credits, which is why the log is read at all.
        let event = r#"{"totalTokens":1200,"agentTimestampMs":1755302400000}"#;
        assert!(newest_quota([event].into_iter()).is_none());
        let q = newest_quota([LOG_LINE].into_iter()).expect("the log line carries a quota");
        assert_eq!(q.pct, Some(42.5));
        assert_eq!(q.tier, "Test Premium");
        assert_eq!(q.kind, "USAGE_PERIOD_TYPE_WEEKLY");
    }

    #[test]
    fn an_int64_written_as_a_string_is_still_a_number() {
        // The server quotes what will not fit in a JSON number. Read as
        // text these read as absent, and the whole block disappears.
        let q = newest_quota([LOG_LINE].into_iter()).expect("the log line carries a quota");
        assert_eq!(q.on_demand_used, Some(3.0));
        assert_eq!(q.on_demand_cap, Some(25.0));
        assert_eq!(q.prepaid, Some(0.0));
    }

    #[test]
    fn the_newest_period_wins_whichever_line_it_is_on() {
        let older = LOG_LINE
            .replace("2026-08-10", "2026-08-03")
            .replace("42.5", "90");
        for lines in [
            [older.as_str(), LOG_LINE],
            [LOG_LINE, older.as_str()],
        ] {
            let q = newest_quota(lines.into_iter()).expect("one of the two is newest");
            assert_eq!(q.pct, Some(42.5));
        }
    }

    #[test]
    fn two_readings_of_one_period_settle_on_the_later_line() {
        // Same window, different percentages: the log repeats the window
        // on every exchange, so later on the file is later in time.
        let earlier = LOG_LINE.replace("42.5", "11");
        let q = newest_quota([earlier.as_str(), LOG_LINE].into_iter()).expect("a quota");
        assert_eq!(q.pct, Some(42.5));
    }

    #[test]
    fn a_running_total_is_counted_as_deltas() {
        // Two events on one day and one on the next. Summed raw this
        // would read 1400 rather than 900, and put all of it on one day.
        let body = concat!(
            r#"{"totalTokens":100,"agentTimestampMs":1755302400000}"#,
            "\n",
            r#"{"totalTokens":400,"agentTimestampMs":1755306000000}"#,
            "\n",
            r#"{"totalTokens":900,"agentTimestampMs":1755388800000}"#,
            "\n",
        );
        let (total, days) = session_days(body);
        assert_eq!(total, 900.0);
        assert_eq!(days.get(&day_of(1755302400000)), Some(&400.0));
        assert_eq!(days.get(&day_of(1755388800000)), Some(&500.0));
    }

    #[test]
    fn a_counter_that_goes_backwards_is_not_spend() {
        // Resuming replays earlier events, so the count can fall. A
        // decrease is a replay, and the recovery back to the high-water
        // mark is not spend either.
        let body = concat!(
            r#"{"totalTokens":500,"agentTimestampMs":1755302400000}"#,
            "\n",
            r#"{"totalTokens":200,"agentTimestampMs":1755302400000}"#,
            "\n",
            r#"{"totalTokens":600,"agentTimestampMs":1755302400000}"#,
            "\n",
        );
        assert_eq!(session_days(body).0, 600.0);
    }

    #[test]
    fn an_event_with_no_timestamp_is_left_out_of_the_day_it_cannot_name() {
        let body = concat!(
            r#"{"totalTokens":100,"agentTimestampMs":1755302400000}"#,
            "\n",
            r#"{"totalTokens":700}"#,
            "\n",
        );
        let (total, days) = session_days(body);
        // The 600 has no day to land in, and it leaves the total with it
        // rather than being banked against whichever day was read last.
        assert_eq!(total, 100.0);
        assert_eq!(days.len(), 1);
    }

    #[test]
    fn the_lane_carries_the_window_it_was_measured_over() {
        let d = Data {
            ok: true,
            quota: newest_quota([LOG_LINE].into_iter()),
            ..Default::default()
        };
        let got = lanes(&d);
        // The fixture's account has a $25 on-demand cap, so it publishes
        // two: the credits and the allowance that is billed.
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].label, "credits");
        assert_eq!(got[0].pct, 42.5);
        assert_eq!(got[0].window_secs, Some(7.0 * 86400.0));
        // The fixture's window closed on 2026-08-17, so by the time anyone
        // runs this it has long since rolled. The reset is therefore a
        // moving target and cannot be pinned to a literal - what can be
        // pinned is that it is in the future, sits on the fixture's own
        // seven-day grid, and is flagged as worked out rather than read.
        let end = iso_epoch("2026-08-17T00:00:00.000000+00:00").unwrap();
        let reset = got[0].reset.unwrap();
        assert!(reset > now(), "the reset is behind us again");
        assert!(reset - now() <= 7.0 * 86400.0, "rolled further than one window");
        let steps = (reset - end) / (7.0 * 86400.0);
        assert!(
            (steps - steps.round()).abs() < 1e-6,
            "the rolled window left the grid the recorded one set: {} windows",
            steps
        );
        assert!(got[0].projected, "a calculated date must say so");
        // From the log, so the percentage belongs to a window that has since
        // closed and the row has to say so. This is the case that was wrong
        // in the wild: 23% shown as current where the account had spent 57%.
        assert!(got[0].stale, "a reading off the disk must not read as live");
    }

    #[test]
    fn an_interval_reads_the_way_someone_would_say_it() {
        assert_eq!(every(3600.0), "1h");
        assert_eq!(every(1800.0), "30m");
        assert_eq!(every(5400.0), "1h 30m");
        assert_eq!(every(7200.0), "2h");
        // Never zero: a config of 0 would otherwise advertise "every 0m",
        // and the poll interval is floored at a minute anyway.
        assert_eq!(every(0.0), "1m");
    }

    #[test]
    fn a_reading_is_judged_by_its_age_not_by_where_it_came_from() {
        // This asserted the opposite until the rule changed: that a reading
        // is current "by definition" when it came from the server. That is
        // the mistake claude.rs already wrote down - it flags the fresher of
        // two readings as the doubtful one - and here it meant a working
        // ping still read "not live", because the answer it fetched was
        // being discarded and an eleven-day-old log line shown instead.
        let base = newest_quota([LOG_LINE].into_iter()).expect("a reading");

        // Taken a minute ago: current, and its window is the one we are in -
        // a live answer is of the week it was asked in, so the log line's
        // week is moved to end after now.
        let live_end = "2099-08-17T00:00:00.000000+00:00";
        let d = Data {
            ok: true,
            quota: Some(Quota {
                taken: Some(now() - 60.0),
                start: "2099-08-10T00:00:00.000000+00:00".into(),
                end: live_end.into(),
                ..base.clone()
            }),
            quota_live: true,
            ..Default::default()
        };
        let got = lanes(&d);
        assert_eq!(got.len(), 2);
        // One reading, so one age: the on-demand lane is marked exactly as
        // the credits lane is, because they came out of the same answer.
        assert!(got.iter().all(|l| !l.stale), "a minute-old reading is current");
        assert!(got.iter().all(|l| !l.projected), "a live window was read, not worked out");
        assert_eq!(
            got[0].reset,
            iso_epoch(live_end),
            "a live window is reported as the server gave it, not rolled"
        );

        // Fetched just now, but of a reading taken days ago. Still old:
        // fetching an old number does not make it a new one.
        let d = Data {
            ok: true,
            quota: Some(Quota { taken: Some(now() - 5.0 * 86400.0), ..base.clone() }),
            quota_live: true,
            ..Default::default()
        };
        assert!(lanes(&d)[0].stale, "a live fetch of an old reading read as current");

        // And the other direction: nothing was fetched, the log supplied it
        // thirty seconds ago, and that is current whatever its source.
        let d = Data {
            ok: true,
            quota: Some(Quota { taken: Some(now() - 30.0), ..base.clone() }),
            quota_live: false,
            ..Default::default()
        };
        assert!(!lanes(&d)[0].stale, "flagged for its source rather than its age");

        // A reading that cannot say when it was taken is treated as old:
        // unknown age is not evidence of youth.
        let d = Data {
            ok: true,
            quota: Some(Quota { taken: None, ..base }),
            quota_live: true,
            ..Default::default()
        };
        assert!(lanes(&d)[0].stale, "an undateable reading passed as current");
    }

    #[test]
    fn an_old_log_pace_is_not_a_live_threshold() {
        // Two hours left of a five-hour window, 80% spent. The window is
        // still open, so there is a cushion, and the log is old enough to
        // be stale. A fresh log of the same window is current whatever its
        // source, so it keeps the live figure.
        let start = now() - 3.0 * 3600.0;
        let end = now() + 2.0 * 3600.0;
        let stamp = |at: f64| {
            chrono::DateTime::from_timestamp(at as i64, 0)
                .expect("a stamp")
                .format("%Y-%m-%dT%H:%M:%S+00:00")
                .to_string()
        };
        let quota = Quota {
            pct: Some(80.0),
            products: Vec::new(),
            taken: Some(now() - (GROK_FRESH_FOR + 60.0)),
            kind: "USAGE_PERIOD_TYPE_WEEKLY".into(),
            start: stamp(start),
            end: stamp(end),
            tier: String::new(),
            on_demand_used: None,
            on_demand_cap: None,
            prepaid: None,
            unified: false,
        };
        let p = palette();
        let old = grok_tab(
            &Data { quota: Some(quota.clone()), quota_live: false, ..Data::default() },
            120,
            &p,
        )
        .join("\n");
        // `~/` is the log path, so the tilde has to be the one on the figure.
        assert!(old.contains("~-20%"), "{old}");
        let threshold = "\x1b[58;2;255;214;140m";
        assert!(!old.contains(threshold), "{old:?}");
        let fresh = grok_tab(
            &Data {
                quota: Some(Quota { taken: Some(now()), ..quota }),
                quota_live: false,
                ..Data::default()
            },
            120,
            &p,
        )
        .join("\n");
        assert!(fresh.contains("-20%"), "{fresh}");
        assert!(!fresh.contains("~-20%"), "{fresh}");
    }

    #[test]
    fn the_paid_allowance_is_a_lane_that_names_its_cap() {
        // The figures were parsed all along and drawn only as a tail on the
        // window line, so the summary that ranks every other allowance on
        // the wall said nothing about the one that costs money.
        let d = Data {
            ok: true,
            quota: newest_quota([LOG_LINE].into_iter()),
            ..Default::default()
        };
        let got = lanes(&d);
        let paid = got
            .iter()
            .find(|l| l.label.starts_with("on-demand"))
            .expect("an on-demand lane");
        // $3 of $25, and the label says which $25 it is - a percentage of
        // an unnamed ceiling is not a number anyone can act on.
        assert_eq!(paid.label, "on-demand $25");
        assert_eq!(paid.pct, 12.0);
        // The credits lane's own window, so the two pace on one clock.
        let credits = got.iter().find(|l| l.label == "credits").expect("a credits lane");
        assert_eq!(paid.window_secs, credits.window_secs);
        assert_eq!(paid.reset, credits.reset);
        assert_eq!(paid.stale, credits.stale);
        assert_eq!(paid.projected, credits.projected);
        assert!(!paid.apart, "it belongs to the same window as the credits");
        // And the tab agrees with it, figures and all.
        let rows = grok_tab(&d, 90, &palette()).join(" ");
        assert!(rows.contains("on-demand $3 of $25"), "{}", rows);
    }

    #[test]
    fn an_account_with_no_on_demand_cap_gets_no_bar_and_is_told_why() {
        // Nought is the state of this account today: no cap set. A 0% bar
        // would say there is an allowance sitting untouched, which is the
        // opposite of what is true - the refusal sand_lane makes for a Bot
        // allowance nobody granted.
        let d = Data {
            ok: true,
            quota: newest_quota([no_cap_line().as_str()].into_iter()),
            ..Default::default()
        };
        assert_eq!(lanes(&d).len(), 1, "a cap of nought invented a lane");
        assert_eq!(lanes(&d)[0].label, "credits");
        let rows = grok_tab(&d, 90, &palette()).join(" ");
        assert!(rows.contains("no on-demand cap set"), "{}", rows);
        assert!(!rows.contains("on-demand $0 of $0"), "{}", rows);
        // And the subscription block agrees. It read "0 of 0 used" here,
        // which is an allowance untouched rather than an allowance absent.
        let plan = plan_block(&d, 90, &palette()).join(" ");
        assert!(plan.contains("no cap set"), "{}", plan);
        assert!(!plan.contains("0 of 0"), "{}", plan);
    }

    #[test]
    fn spend_past_the_cap_keeps_the_real_figures() {
        // Not seen from x.ai yet, and the shape is the same one Cursor's
        // lowered spend limit produces: the figure stays real and unclamped,
        // and the summary draws the bar full and coloured as over.
        let over = LOG_LINE.replace(r#""onDemandUsed":{"val":"3"}"#, r#""onDemandUsed":{"val":"30"}"#);
        let d = Data {
            ok: true,
            quota: newest_quota([over.as_str()].into_iter()),
            ..Default::default()
        };
        let paid = lanes(&d)
            .into_iter()
            .find(|l| l.label.starts_with("on-demand"))
            .expect("an on-demand lane");
        assert_eq!(paid.pct, 120.0, "the percentage was clamped");
        assert_eq!(paid.label, "on-demand $25");
        let rows = grok_tab(&d, 90, &palette()).join(" ");
        assert!(rows.contains("on-demand $30 of $25"), "{}", rows);
    }

    #[test]
    fn a_cap_with_no_credit_figure_still_publishes_its_lane() {
        // Unified-billing accounts get no creditUsagePercent at all. The
        // whole of Grok used to drop off the summary for that, which would
        // now take the billed allowance down with an unrelated absence.
        // Substituting null still leaves the key, which the log used to
        // require; the live path omits it, so the fallback has to as well.
        let omitted = LOG_LINE.replace(r#""creditUsagePercent":42.5,"#, "");
        let d = Data {
            ok: true,
            quota: newest_quota([omitted.as_str()].into_iter()),
            ..Default::default()
        };
        let got = lanes(&d);
        assert_eq!(got.len(), 1, "{:?}", got.iter().map(|l| &l.label).collect::<Vec<_>>());
        assert_eq!(got[0].label, "on-demand $25");
        assert_eq!(got[0].pct, 12.0);
        assert!(d.quota.as_ref().unwrap().pct.is_none(), "an omitted key was invented as nought");

        // A null percentage is the other shape: key present, no number.
        let null = LOG_LINE.replace(r#""creditUsagePercent":42.5"#, r#""creditUsagePercent":null"#);
        let d = Data {
            ok: true,
            quota: newest_quota([null.as_str()].into_iter()),
            ..Default::default()
        };
        let got = lanes(&d);
        assert_eq!(got.len(), 1, "{:?}", got.iter().map(|l| &l.label).collect::<Vec<_>>());
        assert_eq!(got[0].label, "on-demand $25");
        assert_eq!(got[0].pct, 12.0);
    }

    #[test]
    fn the_grok_rows_fit_the_pane_they_were_built_for() {
        // 58 columns is the pane this is read in, and the on-demand row is
        // the widest thing on the quota block now: the label, the bar, the
        // figures and the pace cell all have to sit on one row.
        let p = palette();
        let d = Data {
            ok: true,
            quota: newest_quota([LOG_LINE].into_iter()),
            ..Default::default()
        };
        for w in [40usize, 58, 80, 120] {
            let rows = grok_tab(&d, w, &p);
            for row in &rows {
                let plain = visible(row);
                assert!(
                    plain.chars().count() <= w - 1,
                    "a {}-cell row at width {}: {:?}",
                    plain.chars().count(),
                    w,
                    plain
                );
            }
            // And the bar stands down for the figures rather than the other
            // way round: `seg` clips, so a bar sized without measuring the
            // tail beside it takes the cap off the end of the row instead
            // of overflowing where a width check would catch it.
            if w >= 58 {
                let joined: String = rows.iter().map(|r| visible(r)).collect::<Vec<_>>().join(" ");
                assert!(
                    joined.contains("on-demand $3 of $25"),
                    "the figures were clipped at {}: {:?}",
                    w,
                    joined
                );
            }
        }
    }

    #[test]
    fn a_named_period_with_no_percentage_is_nought_used() {
        // This asserted `None` for one commit, on the reading that x.ai had
        // withdrawn the field for unified-billing accounts. It had not:
        // proto3 omits a scalar sitting at its default, so no key means
        // nought. The answer's own `productUsage` array settles it - the
        // product with usage carries `usagePercent`, the two at nought omit
        // it, in the same array of the same response.
        let body = serde_json::json!({"config": {
            "currentPeriod": {
                "type": "USAGE_PERIOD_TYPE_WEEKLY",
                "start": "2026-08-26T02:09:41.289406+00:00",
                "end": "2026-09-02T02:09:41.289406+00:00"
            },
            "onDemandCap": {"val": 0}, "onDemandUsed": {"val": 0},
            "productUsage": [
                {"product": "GrokBuild"},
                {"product": "GrokChat"},
                {"product": "GrokImagine"}
            ],
        }});
        let q = quota_from(&body).expect("a named period is a reading");
        assert_eq!(q.pct, Some(0.0), "an omitted default read as unknown");

        // But not when the same answer shows use. This fixture once had
        // GrokBuild at 1.0 beside the omitted total and called the total
        // nought - a total of 0% over a product at 1% contradicts itself,
        // and the endpoint has not been seen to send it (it answers 10.0
        // beside 5.0 and 5.0). Review flagged the 0% as fabricated; it is
        // unknown, the same as a total never sent.
        let mut used = body.clone();
        used["config"]["productUsage"][0] = serde_json::json!({"product": "GrokBuild", "usagePercent": 1.0});
        let q = quota_from(&used).expect("still a reading of the period");
        assert_eq!(q.pct, None, "a total invented against the product beside it");
        assert_eq!(q.start, "2026-08-26T02:09:41.289406+00:00");
        assert!(q.taken.is_some(), "a live reading knows when it was taken");

        // A percentage that is present is taken as sent, nought included -
        // a real 0.0 on the wire and an omitted one mean the same thing.
        let mut with = body.clone();
        with["config"]["creditUsagePercent"] = serde_json::json!(1.0);
        assert_eq!(quota_from(&with).unwrap().pct, Some(1.0));
        with["config"]["creditUsagePercent"] = serde_json::json!(0.0);
        assert_eq!(quota_from(&with).unwrap().pct, Some(0.0));

        // An answer naming no period at all is still not a reading: nought
        // is only knowable against a window the server stated.
        assert!(quota_from(&serde_json::json!({"config": {}})).is_none());
        assert!(quota_from(&serde_json::json!({"config": {
            "creditUsagePercent": 5.0
        }}))
        .is_none());
    }

    /// The roll itself, on a fixed clock - `lanes` has to ask the real one.
    #[test]
    fn a_window_that_has_ended_rolls_forward_to_the_one_we_are_in() {
        let day = 86400.0;
        let (begin, end) = (Some(0.0), Some(7.0 * day));
        // Mid-window: nothing to roll, and nothing claimed.
        assert_eq!(window_now(begin, end, 3.0 * day), (Some(7.0 * day), false));
        // One day after it closed: the window we are in ends a week later.
        assert_eq!(window_now(begin, end, 8.0 * day), (Some(14.0 * day), true));
        // Five weeks late still lands on the grid, not five weeks ago.
        assert_eq!(window_now(begin, end, 36.0 * day), (Some(42.0 * day), true));
        // Exactly on a boundary belongs to the window starting there.
        assert_eq!(window_now(begin, end, 14.0 * day), (Some(21.0 * day), true));
    }

    #[test]
    fn a_window_with_no_length_is_left_alone() {
        // Half a reading is not a grid to roll along, and inventing one
        // would put a countdown on screen that nothing supports.
        assert_eq!(window_now(None, Some(100.0), 999.0), (Some(100.0), false));
        assert_eq!(window_now(Some(50.0), None, 999.0), (None, false));
        // A window that does not run forwards is not a window.
        assert_eq!(window_now(Some(100.0), Some(50.0), 999.0), (Some(50.0), false));
        assert_eq!(window_now(Some(50.0), Some(50.0), 999.0), (Some(50.0), false));
    }

    #[test]
    fn an_agent_with_no_quota_publishes_no_lane() {
        assert!(lanes(&Data::default()).is_empty());
        // Present but percentless, on an account with no on-demand cap
        // either: there is no lane to rank, and yet the tier and the
        // billing period are still known, so the reading is kept for the
        // SUBSCRIPTION block rather than thrown away.
        let bare = no_cap_line().replace(r#""creditUsagePercent":42.5"#, r#""creditUsagePercent":null"#);
        let d = Data {
            ok: true,
            quota: newest_quota([bare.as_str()].into_iter()),
            ..Default::default()
        };
        assert!(lanes(&d).is_empty());
        assert_eq!(d.quota.as_ref().map(|q| q.tier.as_str()), Some("Test Premium"));
        assert!(!plan_block(&d, 80, &palette()).is_empty());
    }

    #[test]
    fn the_refresh_is_tried_once_before_the_lapse_and_once_after() {
        let expiry = 1_000_000.0;
        // Long before: nothing to do.
        assert_eq!(refresh_due(expiry, expiry - 3600.0, None), None);
        // Inside the margin: the early attempt, once.
        assert_eq!(refresh_due(expiry, expiry - 300.0, None), Some(false));
        assert_eq!(refresh_due(expiry, expiry - 100.0, Some((expiry, false))), None);
        // Lapsed: the late attempt, once - even though the early one was
        // made. This is the one the CLI acts on, and it was never reached.
        assert_eq!(refresh_due(expiry, expiry + 60.0, Some((expiry, false))), Some(true));
        assert_eq!(refresh_due(expiry, expiry + 7200.0, Some((expiry, true))), None);
        // A late attempt on record also settles the early phase.
        assert_eq!(refresh_due(expiry, expiry - 100.0, Some((expiry, true))), None);
        // A renewed token has a new expiry and the count starts over.
        let renewed = expiry + 21600.0;
        assert_eq!(refresh_due(renewed, renewed + 1.0, Some((expiry, true))), Some(true));
    }

    #[test]
    fn a_lapse_shows_the_last_live_reading_not_an_older_log_line() {
        // What happened: the server had said 12% two hours earlier, the
        // token lapsed, and the row drew 23% from a log line written
        // twenty-seven days before, with a star beside it and a trend
        // computed from it.
        let mut live = Quota { pct: Some(12.0), taken: Some(1_000_000.0), ..Quota::default() };
        let log = Quota { pct: Some(23.0), taken: Some(1_000_000.0 - 27.0 * 86400.0), ..Quota::default() };
        assert_eq!(fresher(Some(live.clone()), Some(log.clone())).and_then(|q| q.pct), Some(12.0));
        // The other way round the log wins: a session since the last ask
        // is newer information.
        let newer_log = Quota { taken: Some(1_000_000.0 + 60.0), ..log.clone() };
        assert_eq!(fresher(Some(live.clone()), Some(newer_log)).and_then(|q| q.pct), Some(23.0));
        // An undated reading never beats a dated one.
        live.taken = None;
        assert_eq!(fresher(Some(live.clone()), Some(log.clone())).and_then(|q| q.pct), Some(23.0));
        // And either alone is better than nothing.
        assert!(fresher(None, Some(log.clone())).is_some());
        assert!(fresher(Some(live), None).is_some());
        assert!(fresher(None, None).is_none());
    }

    #[test]
    fn a_held_live_reading_is_dropped_when_the_account_changes() {
        // The billing body names no account, so the retained success is
        // tagged with the token file's map key. Switching accounts while
        // the pane is open, then failing the first ask, used to keep
        // drawing the previous account's figure.
        let billing = serde_json::json!({
            "config": {
                "creditUsagePercent": 12.0,
                "currentPeriod": {
                    "type": "USAGE_PERIOD_TYPE_WEEKLY",
                    "start": "2026-08-10T00:00:00.000000+00:00",
                    "end": "2026-08-17T00:00:00.000000+00:00"
                }
            }
        });
        let mut caches = Caches::default();
        caches.live.insert(
            LIVE_KEY.to_string(),
            (
                1_000_000.0,
                Some(serde_json::json!({"account": "issuer|acct-a", "billing": billing})),
                f64::MAX,
            ),
        );
        assert_eq!(
            last_live_of(&caches, Some("issuer|acct-a")).and_then(|q| q.pct),
            Some(12.0)
        );
        assert!(
            last_live_of(&caches, Some("issuer|acct-b")).is_none(),
            "a different account must not inherit the previous reading"
        );
        // A missing marker is not a mismatch - same rule as Claude's.
        assert_eq!(last_live_of(&caches, None).and_then(|q| q.pct), Some(12.0));
        assert_eq!(
            parse_token_account(r#"{"issuer|acct-a":{"key":"k"}}"#).as_deref(),
            Some("issuer|acct-a")
        );
        assert_eq!(parse_token_account("{}"), None);
        assert_eq!(parse_token_account(r#"{"other":{"no":"key"}}"#), None);
    }

    #[test]
    fn a_lapsed_token_is_reported_as_lapsed_not_as_missing() {
        // These were one answer - None - and the tab said "not live" for
        // both. Only one of them is the reader's to fix, so they have to
        // read differently.
        // 2001-09-09T01:46:40Z, so the ISO stamps below are readable.
        let at = 1_000_000_000.0;
        let live = serde_json::json!({
            "key": "k", "expires_at": "2001-09-09T01:46:40Z" // at + 0, see below
        });
        // A token with no expiry at all is usable: the server is the judge.
        assert_eq!(token_of(&serde_json::json!({"key": "k"}), at), Ok("k".into()));

        // Lapsed an hour ago.
        let gone = serde_json::json!({"key": "k", "expires_at": "2001-09-09T00:46:40Z"});
        let why = token_of(&gone, at).unwrap_err();
        assert!(why.contains("lapsed"), "{}", why);
        assert!(why.contains("Grok CLI"), "says what refreshes it: {}", why);

        // Still good for an hour.
        let good = serde_json::json!({"key": "k", "expires_at": "2001-09-09T02:46:40Z"});
        assert_eq!(token_of(&good, at), Ok("k".into()));

        // Exactly at the boundary counts as lapsed, not as usable.
        assert!(token_of(&live, at).is_err());

        // No key is a different reason again.
        let why = token_of(&serde_json::json!({"expires_at": "x"}), at).unwrap_err();
        assert!(why.contains("no account"), "{}", why);
    }

    #[test]
    fn a_signed_in_machine_with_no_sessions_still_refreshes_a_lapsing_token() {
        // The empty-session return used to skip the expiry gate, so a
        // fresh install with auth.json and no transcripts asked until the
        // token lapsed and then silently stopped. newest = 0 is that path.
        let at = 1_000_000_000.0;
        let lapsing = at + 60.0;
        assert_eq!(
            refresh_phase(0.0, lapsing, None, at),
            Some(false),
            "no sessions, token inside the margin"
        );
        assert_eq!(
            refresh_phase(0.0, lapsing, Some((lapsing, false)), at),
            None,
            "the same before-phase is not tried twice"
        );
        assert_eq!(
            refresh_phase(0.0, at + 3600.0, None, at),
            None,
            "a token an hour out is left alone"
        );
        // After the lapse the late attempt is still due, even with no
        // transcripts - that is the attempt the CLI actually acts on.
        let gone = at - 1.0;
        assert_eq!(
            refresh_phase(0.0, gone, Some((gone, false)), at),
            Some(true),
            "no sessions, token already lapsed, early attempt on record"
        );
        // A session that is still running keeps the CLI from starting
        // underneath it, same as when transcripts exist.
        assert_eq!(refresh_phase(at - 30.0, lapsing, None, at), None);
        assert_eq!(refresh_phase(at - 200.0, lapsing, None, at), Some(false));
    }

    #[test]
    fn the_badge_says_which_of_the_reasons_applies() {
        // "not live" on its own was the same two words for a lapsed token,
        // a dead endpoint and a null percentage.
        let p = palette();
        let d = Data {
            quota: newest_quota([LOG_LINE].into_iter()),
            quota_live: false,
            quota_at: now() - 30.0,
            quota_every: 300.0,
            quota_why: "the token lapsed 3h ago - the Grok CLI refreshes it".into(),
            ..Data::default()
        };
        let rows = freshness(&d, 110, &p);
        let joined = rows.join("\n");
        assert!(joined.contains("not live"), "{}", joined);
        assert!(joined.contains("token lapsed"), "reason missing: {}", joined);

        // And says nothing extra when the reading is the server's.
        let ok = Data { quota_live: true, quota_why: String::new(), ..d.clone() };
        let joined = freshness(&ok, 110, &p).join("\n");
        assert!(joined.contains("live"), "{}", joined);
        assert!(!joined.contains(" · the"), "invented a reason: {}", joined);
    }

    #[test]
    fn a_narrow_not_live_badge_keeps_the_settings_clause() {
        // Width 20 used to floor the wrap budget, so agent_usage.grok_ping
        // (21) ran past `seg`'s w-1 and the settings key was the half that
        // fell off. Trim each row before joining: a word the pane cannot
        // hold is hard-broken, and the indent would hide the join.
        let p = palette();
        let d = Data {
            quota: newest_quota([LOG_LINE].into_iter()),
            quota_live: false,
            quota_at: 0.0,
            quota_every: 0.0,
            ..Data::default()
        };
        let rows = freshness(&d, 20, &p);
        for row in &rows {
            let vis = visible(row);
            assert!(vis.chars().count() <= 20, "overflow at 20: {vis:?}");
        }
        let glued: String = rows.iter().map(|r| visible(r).trim().to_string()).collect();
        assert!(glued.contains("grok_ping"), "{glued}");
        assert!(glued.contains("`,`"), "{glued}");
    }

    #[test]
    fn the_tab_carries_its_subscription_at_every_width_the_wall_uses() {
        // The bar's width is what is left after the labels, and this pane
        // is dragged narrow on a phone. A panicking tab takes the whole
        // widget with it, and a dropped SUBSCRIPTION leaves a percentage
        // that is a percentage of nothing stated.
        let p = palette();
        let d = Data {
            ok: true,
            history: Default::default(),
            sessions: 2,
            files: 3,
            total: 12_345.0,
            daily: HashMap::from([(NaiveDate::from_ymd_opt(2026, 8, 16).unwrap(), 900.0)]),
            last: now(),
            quota: newest_quota([LOG_LINE].into_iter()),
            quota_live: false,
            quota_at: 0.0,
            quota_every: 0.0,
            quota_why: String::new(),
            spacexsi: Spacexsi::No,
        };
        for w in [40usize, 80, 200] {
            let rows = tab(&d, w, 24, &Config::default(), &p);
            assert!(rows.iter().any(|r| r.contains("WEEKLY QUOTA")), "width {}", w);
            assert!(rows.iter().any(|r| r.contains("SUBSCRIPTION")), "width {}", w);
        }
    }

    #[test]
    fn a_machine_with_no_sessions_still_shows_the_credit_window() {
        // The account has a quota and this disk has no transcripts. Both
        // facts are true and the tab states both: the window is drawn, and
        // the sections that count local work say there is none. Before this
        // the log was never read, so the tab said only "No Grok sessions"
        // and the summary screen listed Grok as publishing no quota at all.
        let p = palette();
        let d = Data {
            ok: false,
            quota: newest_quota([LOG_LINE].into_iter()),
            ..Data::default()
        };
        let rows = grok_tab(&d, 90, &p).join(" ");
        assert!(rows.contains("QUOTA"), "{}", rows);
        assert!(rows.contains("42%") || rows.contains("43%"), "{}", rows);
        assert!(rows.contains("No Grok sessions"), "{}", rows);
        // And it publishes both lanes, so the summary does not disagree
        // with the tab about what Grok has: credits, and the paid
        // allowance the tab now draws a bar for.
        assert_eq!(lanes(&d).len(), 2);
        assert!(rows.contains("on-demand $3 of $25"), "{}", rows);
    }

    #[test]
    fn a_period_the_server_did_not_name_is_still_labelled() {
        assert_eq!(period_name("USAGE_PERIOD_TYPE_WEEKLY"), "weekly");
        assert_eq!(period_name("USAGE_PERIOD_TYPE_MONTHLY"), "monthly");
        assert_eq!(period_name(""), "current");
    }

    #[test]
    fn a_turn_is_filed_under_the_day_it_was_that_day_here() {
        // 1755388200000 is 23:30 UTC. East of Greenwich that is already
        // tomorrow locally, west of it the same evening - either way the
        // reader must agree with the wall clock of whoever is reading the
        // pane, because the three calendars drawn beside this one do.
        let body = concat!(
            r#"{"totalTokens":0,"agentTimestampMs":1755388200000}"#,
            "\n",
            r#"{"totalTokens":700,"agentTimestampMs":1755388200000}"#,
            "\n",
        );
        let (_, days) = session_days(body);
        let local = Local
            .timestamp_millis_opt(1755388200000)
            .single()
            .expect("a fixed timestamp")
            .date_naive()
            .to_string();
        assert_eq!(days.get(&local), Some(&700.0));
        // And nothing was filed under the UTC day, unless this machine is
        // on UTC and the two are the same date.
        let utc = chrono::Utc
            .timestamp_millis_opt(1755388200000)
            .single()
            .expect("a fixed timestamp")
            .date_naive()
            .to_string();
        if utc != local {
            assert_eq!(days.get(&utc), None);
        }
    }

    /// The day the reader will file a stamp under - local, matching the
    /// reader, so this asserts the bucketing rather than the zone this
    /// machine happens to run in.
    fn day_of(ms: i64) -> String {
        Local
            .timestamp_millis_opt(ms)
            .single()
            .expect("a fixed timestamp")
            .date_naive()
            .to_string()
    }

    #[test]
    fn a_missing_quota_says_which_step_failed() {
        let off = why_no_lane(&Data::default());
        assert!(off.contains("grok_ping"), "{off}");
        assert!(off.contains("creditUsagePercent"), "{off}");
        assert!(off.contains(tc::SET_IN_SETTINGS), "{off}");

        let bare = no_cap_line().replace(r#""creditUsagePercent":42.5"#, r#""creditUsagePercent":null"#);
        let live_blank = Data {
            quota: newest_quota([bare.as_str()].into_iter()),
            quota_live: true,
            quota_every: 300.0,
            ..Data::default()
        };
        let note = why_no_lane(&live_blank);
        assert!(note.contains("x.ai answered"), "{note}");
        assert!(note.contains("no credit figure"), "{note}");

        let lapsed = Data {
            quota_why: "the token lapsed 3h ago - the Grok CLI refreshes it".into(),
            quota_every: 300.0,
            ..Data::default()
        };
        let note = why_no_lane(&lapsed);
        assert!(note.contains("token lapsed"), "{note}");
    }

    // The unified Cursor and Grok plan, as the credits endpoint and the log
    // answered on 2026-10-13. Shapes and figures only.

    /// The credits endpoint on the unified plan, around now.
    fn unified_body() -> serde_json::Value {
        let stamp = |at: f64| {
            chrono::DateTime::from_timestamp_millis((at * 1000.0) as i64)
                .expect("a stamp")
                .format("%Y-%m-%dT%H:%M:%S%.3f+00:00")
                .to_string()
        };
        let (start, end) = (now() - 86400.0, now() + 6.0 * 86400.0);
        serde_json::json!({"config": {
            "currentPeriod": {"type": "USAGE_PERIOD_TYPE_WEEKLY", "start": stamp(start), "end": stamp(end)},
            "creditUsagePercent": 5.0,
            "productUsage": [{"usagePercent": 3.0}, {"product": "GrokBot", "usagePercent": 2.0}],
            "prepaidBalance": {"val": 2000},
            "topUpMethod": "TOP_UP_METHOD_SAVED_PAYMENT_METHOD",
            "billingPeriodStart": stamp(start),
            "billingPeriodEnd": stamp(end)
        }})
    }

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

    pub(super) fn unified_grok(place: Spacexsi) -> Data {
        Data {
            quota: quota_from(&unified_body()),
            quota_live: true,
            quota_every: 300.0,
            spacexsi: place,
            ..Data::default()
        }
    }

    #[test]
    fn the_credits_endpoint_marks_the_unified_plan_and_states_the_balance_in_cents() {
        let q = quota_from(&unified_body()).expect("a reading");
        assert!(q.unified, "topUpMethod and a GrokBot product are the unified plan's");
        // 2000 is $20 - the balance Cursor states as availableCents "2000".
        assert_eq!(q.prepaid, Some(20.0));
        assert_eq!(q.pct, Some(5.0));
        // A product with no name is kept and not guessed at, so the split
        // still adds up to the week.
        assert_eq!(q.products, vec![(String::new(), 3.0), ("GrokBot".to_string(), 2.0)]);
        // On the unified plan the unnamed share is Cursor's, and the split
        // is whole percents.
        let d = unified_grok(Spacexsi::Shared);
        assert_eq!(pool_split(&d), vec![("Cursor".to_string(), 3.0), ("Grok Bot".to_string(), 2.0)]);
        // On the account's own plan nothing is renamed.
        assert_eq!(product_name("", false), "unnamed");
        assert_eq!(product_name("GrokBuild", false), "GrokBuild");
        assert_eq!(product_name("GrokBuild", true), "Grok Build");
        assert_eq!(product_name("GrokImagine", true), "Grok Imagine");
        // Either marker is enough on its own.
        let mut only_topup = unified_body();
        only_topup["config"].as_object_mut().unwrap().remove("productUsage");
        assert!(quota_from(&only_topup).unwrap().unified);
        let mut only_bot = unified_body();
        only_bot["config"].as_object_mut().unwrap().remove("topUpMethod");
        assert!(quota_from(&only_bot).unwrap().unified);
    }

    #[test]
    fn is_unified_billing_user_alone_is_not_the_unified_plan() {
        // The log carried it as true on a Free tier, with a window of its
        // own, before the plans merged.
        let line = concat!(
            r#"{"msg":"billing: fetched credits config","ctx":{"subscriptionTier":"Free","config":{"#,
            r#""currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY","#,
            r#""start":"2026-10-07T02:09:41.289406+00:00","end":"2026-10-14T02:09:41.289406+00:00"},"#,
            r#""isUnifiedBillingUser":true,"onDemandCap":{"val":0},"onDemandUsed":{"val":0},"#,
            r#""prepaidBalance":{"val":0}}}}"#
        );
        let q = newest_quota([line].into_iter()).expect("a reading");
        assert!(!q.unified);
    }

    #[test]
    fn the_newest_window_wins_across_the_merge() {
        // The log holds the account's own window and then the unified one;
        // the second is the account now. It carries no marker - the log
        // never does - and its balance is cents like the endpoint's.
        let before = concat!(
            r#"{"ts":"2026-10-12T07:46:27.663Z","ctx":{"subscriptionTier":null,"config":{"#,
            r#""creditUsagePercent":2.0,"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY","#,
            r#""start":"2026-10-11T04:13:45.862845+00:00","end":"2026-10-18T04:13:45.862845+00:00"},"#,
            r#""isUnifiedBillingUser":true,"onDemandCap":{"val":0},"onDemandUsed":{"val":0},"#,
            r#""prepaidBalance":{"val":0}}}}"#
        );
        let after = concat!(
            r#"{"ts":"2026-10-13T04:16:10.188Z","ctx":{"subscriptionTier":"X Premium+","config":{"#,
            r#""creditUsagePercent":4.0,"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY","#,
            r#""start":"2026-10-13T02:39:23.429+00:00","end":"2026-10-20T02:39:23.429+00:00"},"#,
            r#""isUnifiedBillingUser":true,"prepaidBalance":{"val":2000}}}}"#
        );
        for lines in [[before, after], [after, before]] {
            let q = newest_quota(lines.into_iter()).expect("a reading");
            assert_eq!(q.start, "2026-10-13T02:39:23.429+00:00");
            assert_eq!(q.pct, Some(4.0));
            assert_eq!(q.prepaid, Some(20.0));
            assert!(q.on_demand_cap.is_none(), "the unified plan has no x.ai cap");
        }
        // The pool's window to the millisecond, which is what Cursor's is
        // matched against. Read at a moment inside that week: the clock
        // rolls a closed window forward, and this one closed on Oct 20.
        let d = Data { quota: newest_quota([before, after].into_iter()), ..Data::default() };
        let inside = 1_792_000_000.0; // 2026-10-14, between start and end
        let (s, e) = pool_window_at(&d, inside).expect("a window");
        assert_eq!((s.round(), e.round()), (1_791_859_163_429.0, 1_792_463_963_429.0));
    }

    #[test]
    fn a_unified_grok_goes_under_spacexsi_and_its_own_plan_does_not() {
        let alone = grok_tab(&unified_grok(Spacexsi::Alone), 120, &palette())
            .iter().map(|r| strip(r)).collect::<Vec<_>>().join("\n");
        for want in ["SpaceXSI", "weekly", "of the weekly pool", "prepaid $20", "bills to prepaid credits"] {
            assert!(alone.contains(want), "missing {want:?}: {alone}");
        }
        for gone in ["WEEKLY QUOTA", "credits used", "no on-demand cap set", "$2000"] {
            assert!(!alone.contains(gone), "{gone:?}: {alone}");
        }
        // Shared, the balance and the limit are the CURSOR tab's to show.
        let shared = grok_tab(&unified_grok(Spacexsi::Shared), 120, &palette())
            .iter().map(|r| strip(r)).collect::<Vec<_>>().join("\n");
        assert!(shared.contains("same account as the CURSOR tab"), "{shared}");
        assert!(!shared.contains("prepaid"), "the balance stated twice: {shared}");
        let whole = tab(&unified_grok(Spacexsi::Shared), 120, 24, &Config::default(), &palette())
            .iter().map(|r| strip(r)).collect::<Vec<_>>().join("\n");
        assert!(!whole.contains("prepaid balance"), "the SUBSCRIPTION block restated it: {whole}");
        let separate = grok_tab(&unified_grok(Spacexsi::Separate), 120, &palette())
            .iter().map(|r| strip(r)).collect::<Vec<_>>().join("\n");
        assert!(separate.contains("SpaceXSI ── via Grok"), "{separate}");
        // The same reading on an account Grok cannot tell is unified, and no
        // window match: the heading it always had.
        let own = grok_tab(&unified_grok(Spacexsi::No), 120, &palette())
            .iter().map(|r| strip(r)).collect::<Vec<_>>().join("\n");
        assert!(own.contains("WEEKLY QUOTA") && !own.contains("SpaceXSI"), "{own}");
    }

    #[test]
    fn a_spacexsi_grok_tab_draws_no_local_session_data() {
        let mut d = Data::unified_fixture();
        d.spacexsi = Spacexsi::Alone;
        let text = tab(&d, 120, 24, &Config::default(), &palette()).iter().map(|r| strip(r)).collect::<Vec<_>>().join("\n");
        assert!(text.contains("SpaceXSI"), "{text}");
        for local in ["TOTALS", "TOKENS / DAY", "session files"] {
            assert!(!text.contains(local), "local data {local:?} under SpaceXSI: {text}");
        }
        // Its own plan still draws them.
        d.spacexsi = Spacexsi::No;
        let own = tab(&d, 120, 24, &Config::default(), &palette()).join("\n");
        assert!(own.contains("TOTALS") && own.contains("TOKENS / DAY"), "{own}");
    }

    #[test]
    fn a_just_reset_unified_log_line_still_reaches_the_reader() {
        // Review: nought usage omits creditUsagePercent and the unified plan
        // has no onDemandCap, so the line was skipped before it was parsed.
        let line = concat!(
            r#"{"ts":"2026-10-13T02:40:00.000Z","ctx":{"config":{"#,
            r#""currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY","#,
            r#""start":"2026-10-13T02:39:23.429+00:00","end":"2026-10-20T02:39:23.429+00:00"},"#,
            r#""isUnifiedBillingUser":true,"prepaidBalance":{"val":2000}}}}"#
        );
        let q = newest_quota([line].into_iter()).expect("the line was skipped");
        assert_eq!(q.start, "2026-10-13T02:39:23.429+00:00");
        assert_eq!(q.prepaid, Some(20.0));
    }

    #[test]
    fn the_product_split_wraps_whole_entries_on_a_narrow_pane() {
        // Review: one joined row let `seg` clip `Grok Bot ~2%` off a
        // 26-column pane.
        let entries = vec!["Cursor ~3%".to_string(), "Grok Bot ~2%".to_string(), "Grok Imagine ~1%".to_string()];
        for w in [26usize, 40, 120] {
            let rows: Vec<String> = by_product_rows(entries.clone(), w, &palette()).iter().map(|r| strip(r)).collect();
            let text = rows.join("\n");
            for entry in &entries {
                assert!(text.contains(entry.as_str()), "{entry:?} cut at {w}: {text}");
            }
            for row in &rows {
                assert!(row.chars().count() <= w - 1, "{w}: {row:?}");
            }
        }
        assert_eq!(by_product_rows(entries, 120, &palette()).len(), 1, "a wide pane keeps one row");
    }

    #[test]
    fn a_cached_live_answer_past_its_reset_is_projected_like_a_log_line() {
        // Review: a live answer held across the weekly reset kept last
        // week's figure as an unqualified current lane in [+].
        let mut d = unified_grok(Spacexsi::Shared);
        let gone = chrono::DateTime::from_timestamp((now() - 60.0) as i64, 0).unwrap();
        let begun = chrono::DateTime::from_timestamp((now() - 60.0 - 7.0 * 86400.0) as i64, 0).unwrap();
        let q = d.quota.as_mut().unwrap();
        q.start = begun.format("%Y-%m-%dT%H:%M:%S%.3f+00:00").to_string();
        q.end = gone.format("%Y-%m-%dT%H:%M:%S%.3f+00:00").to_string();
        let lane = lanes(&d).into_iter().next().expect("a lane");
        assert!(lane.projected, "last week's figure drawn as this week's");
        assert!(lane.reset.unwrap() > now(), "the reset left in the past");
        // Review: the tab's heading rolls it the same way, so its countdown
        // is to the reset coming rather than gone.
        let tab = tab_with_plots(&d, 120, 40, &Config::default(), &palette(), &mut Vec::new())
            .iter()
            .map(|r| strip(r))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(tab.contains("resets in ~"), "{tab}");
        // A live answer of the week we are in is taken as sent.
        assert!(!lanes(&unified_grok(Spacexsi::Shared))[0].projected);
    }

    #[test]
    fn only_a_live_recent_reading_is_current() {
        // Review: a split from a request that has since failed was drawn
        // under Cursor's `live` heading.
        let d = unified_grok(Spacexsi::Shared);
        assert!(reading_is_current(&d));
        let log = Data { quota_live: false, ..unified_grok(Spacexsi::Shared) };
        assert!(!reading_is_current(&log), "a log reading");
        let mut old = unified_grok(Spacexsi::Shared);
        old.quota.as_mut().unwrap().taken = Some(now() - GROK_FRESH_FOR - 60.0);
        assert!(!reading_is_current(&old), "an old reading");
        // Review: a fresh answer held in the cache past its week's reset.
        let mut ended = unified_grok(Spacexsi::Shared);
        let gone = chrono::DateTime::from_timestamp((now() - 60.0) as i64, 0).unwrap();
        ended.quota.as_mut().unwrap().end = gone.format("%Y-%m-%dT%H:%M:%S%.3f+00:00").to_string();
        assert!(!reading_is_current(&ended), "a week that has ended");
    }

    #[test]
    fn an_omitted_total_beside_a_product_in_use_is_unknown_not_nought() {
        // Review (P1): proto3 omits a total at nought, but a total omitted
        // while a product reports use is a total the answer did not give.
        let period = serde_json::json!({
            "type": "USAGE_PERIOD_TYPE_WEEKLY",
            "start": "2026-10-13T02:39:23.429+00:00", "end": "2026-10-20T02:39:23.429+00:00"
        });
        let used = serde_json::json!({"config": {
            "currentPeriod": period,
            "productUsage": [{"product": "GrokBuild", "usagePercent": 1.0}, {"product": "GrokChat"}]
        }});
        assert_eq!(quota_from(&used).unwrap().pct, None, "a fabricated 0%");
        // Nothing reports use: nought is the real reading, as before.
        let idle = serde_json::json!({"config": {
            "currentPeriod": period,
            "productUsage": [{"product": "GrokBuild"}, {"product": "GrokChat"}]
        }});
        assert_eq!(quota_from(&idle).unwrap().pct, Some(0.0));
        // And no lane is drawn for an unknown total.
        let d = Data { quota: quota_from(&used), quota_live: true, spacexsi: Spacexsi::Alone, ..Data::default() };
        assert!(lanes(&d).is_empty(), "a lane for an unknown total");
    }

    #[test]
    fn a_stale_window_is_rolled_onto_this_week_before_it_is_matched() {
        // Review: an old reading's window was compared as recorded, so it
        // could never match the same account's current Cursor window.
        let stamp = |at: f64| {
            chrono::DateTime::from_timestamp_millis((at * 1000.0) as i64)
                .unwrap()
                .format("%Y-%m-%dT%H:%M:%S%.3f+00:00")
                .to_string()
        };
        let week = 7.0 * 86400.0;
        let (start, end) = (now() - 2.5 * week, now() - 1.5 * week);
        let mut d = unified_grok(Spacexsi::Shared);
        d.quota_live = false;
        let q = d.quota.as_mut().unwrap();
        q.start = stamp(start);
        q.end = stamp(end);
        let (s, e) = pool_window(&d).expect("a window");
        assert!((e - s - week * 1000.0).abs() < 1.0, "the week's length kept");
        assert!(s / 1000.0 <= now() && now() < e / 1000.0, "rolled onto the week we are in");
        assert!(((e / 1000.0 - end) / week - 2.0).abs() < 1e-6, "rolled by whole weeks");
        // Review: a live answer held in the poll cache can outlast its
        // week too, and rolls the same way.
        d.quota_live = true;
        assert_eq!(pool_window(&d), Some((s, e)));
        // A window still running is taken as sent.
        let q = d.quota.as_mut().unwrap();
        q.start = stamp(now() - 0.5 * week);
        q.end = stamp(now() + 0.5 * week);
        let (s, e) = pool_window(&d).unwrap();
        assert!((s / 1000.0 - (now() - 0.5 * week)).abs() < 1.0 && (e / 1000.0 - (now() + 0.5 * week)).abs() < 1.0);
    }

    #[test]
    fn the_unified_lane_is_named_as_cursor_names_the_pool() {
        let labels = |place| lanes(&unified_grok(place)).into_iter().map(|l| l.label).collect::<Vec<_>>();
        assert_eq!(labels(Spacexsi::Alone), ["weekly pool"]);
        assert_eq!(labels(Spacexsi::No), ["credits"]);
    }

    #[test]
    fn every_unified_grok_row_fits_its_pane() {
        for place in [Spacexsi::No, Spacexsi::Alone, Spacexsi::Shared, Spacexsi::Separate] {
            for w in [26usize, 30, 40, 58, 80, 120] {
                for row in grok_tab(&unified_grok(place), w, &palette()) {
                    let n = strip(&row).chars().count();
                    assert!(n <= w - 1, "{n} wide at {w} ({place:?}): {:?}", strip(&row));
                }
            }
        }
    }
}

/// Readings for tests outside this module: the unified plan's credits
/// answer, and the account's own plan as its log recorded it.
#[cfg(test)]
impl Data {
    pub fn unified_fixture() -> Data {
        // With sessions on this machine, so a test can see what the merged
        // tab does with them.
        Data {
            ok: true,
            sessions: 2,
            files: 2,
            total: 1_000.0,
            daily: HashMap::from([(Local::now().date_naive(), 1_000.0)]),
            last: now(),
            ..tests::unified_grok(Spacexsi::No)
        }
    }

    /// The same reading with nothing polling x.ai - a log figure.
    pub fn polling_nobody(mut d: Data) -> Data {
        d.quota_every = 0.0;
        d.quota_live = false;
        d
    }

    pub fn own_plan_fixture() -> Data {
        Data {
            quota: newest_quota([tests::LOG_LINE].into_iter()),
            quota_every: 300.0,
            ..Data::default()
        }
    }
}

#[cfg(test)]
fn grok_tab(d: &Data, w: usize, p: &Palette) -> Vec<String> {
    grok_tab_with_plots(d, w, p, &mut Vec::new())
}

#[cfg(test)]
pub fn tab(d: &Data, w: usize, _h: usize, _cfg: &Config, p: &Palette) -> Vec<String> {
    tab_with_plots(d, w, _h, _cfg, p, &mut Vec::new())
}
