// terminal-toys - small dependency-free terminal widgets
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

use std::collections::{HashMap, HashSet};

use chrono::{Datelike, NaiveDate, TimeZone, Utc};
use toys_core as tc;

use crate::shared::*;
use crate::*;

/// Where the CLI keeps its transcripts. Nested a couple of levels down, so
/// it is walked rather than listed.
const SESSIONS: &str = ".grok/sessions";
/// The quota is not in the session transcripts: it arrives on the client
/// log, which the CLI writes as it talks to the server.
const LOG: &str = ".grok/logs/unified.jsonl";
/// The credit reading is one line among the client log's chatter and is
/// only rewritten when the server sends a new one, so the tail has to be
/// long enough to still contain one after a busy session.
const LOG_TAIL: u64 = 2 * 1024 * 1024;
/// Namespace for this reader's per-file cache entries, so the prune below
/// can find its own and leave every other reader's alone.
const CACHE: &str = "grok:session:";

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
    kind: String,
    start: String,
    end: String,
    tier: String,
    on_demand_used: Option<f64>,
    on_demand_cap: Option<f64>,
    prepaid: Option<f64>,
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
    /// Newest transcript mtime, as epoch seconds.
    last: f64,
    quota: Option<Quota>,
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
fn session_days(body: &str) -> (f64, HashMap<String, f64>) {
    let (mut total, mut prev) = (0.0f64, 0.0f64);
    let mut days: HashMap<String, f64> = HashMap::new();
    for line in body.lines() {
        let Some(value) = int_after(line, "\"totalTokens\":") else {
            continue;
        };
        let step = value - prev;
        prev = prev.max(value);
        if step <= 0.0 {
            continue;
        }
        // A step whose event carries no timestamp is dropped rather than
        // banked against whichever day was read last: a calendar that
        // invents a day is worse than one that is short by a step.
        let Some(ms) = int_after(line, "\"agentTimestampMs\":") else {
            continue;
        };
        let Some(at) = Utc.timestamp_millis_opt(ms as i64).single() else {
            continue;
        };
        *days.entry(at.date_naive().to_string()).or_insert(0.0) += step;
        total += step;
    }
    (total, days)
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
        // few that carry a credit reading costs more than the read.
        if !line.contains("creditUsagePercent") {
            continue;
        }
        let Ok(d) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let cfg = &d["ctx"]["config"];
        // The key has to be there, but it does not have to hold a number:
        // a reading the server sent with a null percentage still names the
        // tier and the billing period, and those rows are real.
        if !cfg
            .as_object()
            .is_some_and(|o| o.contains_key("creditUsagePercent"))
        {
            continue;
        }
        let period = &cfg["currentPeriod"];
        let got = Quota {
            pct: val_of(&cfg["creditUsagePercent"]),
            kind: text(period, "type"),
            start: text(period, "start"),
            end: text(period, "end"),
            tier: text(&d["ctx"], "subscriptionTier"),
            on_demand_used: val_of(&cfg["onDemandUsed"]["val"]),
            on_demand_cap: val_of(&cfg["onDemandCap"]["val"]),
            prepaid: val_of(&cfg["prepaidBalance"]["val"]),
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
pub fn read(caches: &mut Caches) -> Data {
    use std::os::unix::fs::MetadataExt;
    let mut files = Vec::new();
    walk(&under_home(SESSIONS), "updates.jsonl", &mut files);
    if files.is_empty() {
        // The log is not read either. The tab this feeds leads with its
        // transcripts and stops when there are none, so a credit window
        // read here would have nowhere to be drawn.
        return Data::default();
    }
    let (mut total, mut sessions, mut newest) = (0.0f64, 0usize, 0.0f64);
    let mut daily: HashMap<NaiveDate, f64> = HashMap::new();
    let mut seen: HashSet<String> = HashSet::new();
    for path in &files {
        let Ok(meta) = std::fs::metadata(path) else {
            continue;
        };
        newest = newest.max(meta.mtime() as f64);
        // Keyed on the file's own (mtime, size): a transcript that has not
        // been appended to cannot have different deltas, and re-reading
        // every session on every refresh to learn that is the whole cost
        // of this tab.
        let key = format!("{}{}:{}:{}", CACHE, path, meta.mtime(), meta.size());
        let got = cached(caches, &key, PLAN_TTL, || {
            let (total, days) = session_days(&std::fs::read_to_string(path).ok()?);
            Some(serde_json::json!({"total": total, "daily": days}))
        });
        seen.insert(key);
        let Some(got) = got else {
            continue;
        };
        let spent = got["total"].as_f64().unwrap_or(0.0);
        if spent <= 0.0 {
            continue;
        }
        sessions += 1;
        total += spent;
        for (day, n) in got["daily"].as_object().into_iter().flatten() {
            let Ok(at) = NaiveDate::parse_from_str(day, "%Y-%m-%d") else {
                continue;
            };
            *daily.entry(at).or_insert(0.0) += n.as_f64().unwrap_or(0.0);
        }
    }
    // What makes a reading safe to trust - that its key names the size and
    // mtime it was read at - is also what makes it dead the moment the
    // session appends a line. Without this the live session's old readings
    // pile up for as long as the pane runs.
    caches
        .live
        .retain(|key, _| !key.starts_with(CACHE) || seen.contains(key));
    Data {
        ok: true,
        sessions,
        files: files.len(),
        total,
        daily,
        last: newest,
        quota: newest_quota(tail_lines(&under_home(LOG), LOG_TAIL).iter().map(String::as_str)),
    }
}

/// Every quota this agent publishes, for the summary screen.
///
/// One lane: the credit window is the only allowance Grok states. The
/// window length is offered only when both ends parse and run forwards,
/// but the reset is offered whenever the end does - a countdown is
/// readable without knowing how long the window was.
pub fn lanes(d: &Data) -> Vec<Lane> {
    let Some(q) = d.quota.as_ref() else {
        return Vec::new();
    };
    let Some(pct) = q.pct else {
        return Vec::new();
    };
    let (begin, end) = (iso_epoch(&q.start), iso_epoch(&q.end));
    vec![Lane {
        label: "credits".into(),
        pct,
        window_secs: match (begin, end) {
            (Some(b), Some(e)) if e > b => Some(e - b),
            _ => None,
        },
        reset: end,
        stale: false,
    }]
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

fn grok_tab(d: &Data, w: usize, p: &Palette) -> Vec<String> {
    if !d.ok {
        return no_local("No Grok sessions on this machine.", run_hint("grok"), w, p);
    }
    let hue = agent_hue("grok");
    let mut rows: Vec<String> = Vec::new();
    let quota = d.quota.as_ref().filter(|q| q.pct.is_some());
    if let Some(q) = quota {
        let pct = q.pct.unwrap_or(0.0);
        // The one real remaining-quota figure in this widget: everything
        // else here counts what was spent. It leads the tab for that
        // reason.
        let (begin, end) = (iso_epoch(&q.start), iso_epoch(&q.end));
        let left = end.map(|e| (e - now()) / 86400.0).filter(|days| *days >= 0.0);
        rows.push(tc::seg(
            &[
                (
                    p.lbl.as_str(),
                    format!(" ── {} QUOTA ── ", period_name(&q.kind).to_uppercase()),
                ),
                (
                    p.dim.as_str(),
                    left.map(|days| format!("resets in {:.1} days", days))
                        .unwrap_or_default(),
                ),
            ],
            w - 1,
        ));
        // A window is only a window if it runs forwards; without both ends
        // there is no pace to report, and a mark placed anyway would be a
        // claim about a clock nobody read.
        let (span, reset) = match (begin, end) {
            (Some(b), Some(e)) if e > b => (Some(e - b), Some(e)),
            _ => (None, None),
        };
        let mut line: Vec<(String, String)> = vec![(
            pct_colour(pct, hue, p),
            format!(" {:<5}", format!("{:.0}%", pct)),
        )];
        line.extend(paced_bar(
            (pct / 100.0).clamp(0.0, 1.0),
            elapsed_of(span, reset),
            w.saturating_sub(38).max(10),
            hue,
            p,
        ));
        line.push((p.dim.clone(), "  credits used".into()));
        line.push(pace_cell(lead(pct, span, reset), p));
        rows.push(seg_of(&line, w));

        let (from, to) = (short_day(&q.start), short_day(&q.end));
        let window = if from.is_empty() || to.is_empty() {
            "?".to_string()
        } else {
            format!("{} → {}", from, to)
        };
        let mut extras: Vec<String> = Vec::new();
        if let Some(cap) = q.on_demand_cap.filter(|v| *v != 0.0) {
            extras.push(format!(
                "on-demand {}/{}",
                q.on_demand_used.unwrap_or(0.0),
                cap
            ));
        }
        if let Some(prepaid) = q.prepaid.filter(|v| *v != 0.0) {
            extras.push(format!("prepaid {}", prepaid));
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
        rows.push(String::new());
    }

    rows.push(tc::seg(
        &[
            (p.lbl.as_str(), " ── TOTALS ── ".into()),
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
    if quota.is_some() {
        note.push_str(
            " The quota above is the server's own figure, read from the client log - not \
             inferred.",
        );
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
    // A cap of zero is still a cap the account has, so this asks whether
    // the server sent one rather than whether it is spendable.
    if let Some(cap) = q.on_demand_cap {
        pairs.push((
            "on-demand".into(),
            format!("{} of {} used", q.on_demand_used.unwrap_or(0.0), cap),
        ));
    }
    if let Some(prepaid) = q.prepaid {
        pairs.push(("prepaid balance".into(), format!("{}", prepaid)));
    }
    plan_rows(&q.tier, &pairs, w, "", None, "", p)
}

/// The whole tab: the credit window, what the transcripts recorded, and
/// which subscription that percentage is a percentage of.
///
/// No METERED section, unlike the other tabs. Grok's events carry one
/// running total with no model on it and no split by priced kind, and
/// input, output and the two cache durations differ in price by up to
/// fifty times - so a total here cannot be costed at all.
pub fn tab(d: &Data, w: usize, _h: usize, _cfg: &Config, p: &Palette) -> Vec<String> {
    add_section(grok_tab(d, w, p), plan_block(d, w, p))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A client-log line carrying a credit reading. Invented, but shaped
    /// like the real thing down to the microseconds and the numeric offset
    /// on the period - a fixture that arrives in a tidier format than the
    /// server sends tests a parser nobody has.
    const LOG_LINE: &str = concat!(
        r#"{"msg":"config","lvl":"info","ctx":{"subscriptionTier":"Test Premium","config":{"#,
        r#""creditUsagePercent":42.5,"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY","#,
        r#""start":"2026-08-10T00:00:00.000000+00:00","#,
        r#""end":"2026-08-17T00:00:00.000000+00:00"},"#,
        r#""onDemandUsed":{"val":"3"},"onDemandCap":{"val":25},"#,
        r#""prepaidBalance":{"val":0}}}}"#
    );

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
        // Two events on one UTC day and one on the next. Summed raw this
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
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].label, "credits");
        assert_eq!(got[0].pct, 42.5);
        assert_eq!(got[0].window_secs, Some(7.0 * 86400.0));
        assert_eq!(got[0].reset, iso_epoch("2026-08-17T00:00:00.000000+00:00"));
        // Never cached: this was read from a file on this machine a moment
        // ago, so counting it down is honest.
        assert!(!got[0].stale);
    }

    #[test]
    fn an_agent_with_no_quota_publishes_no_lane() {
        assert!(lanes(&Data::default()).is_empty());
        // Present but percentless: there is no lane to rank, and yet the
        // tier and the billing period are still known, so the reading is
        // kept for the SUBSCRIPTION block rather than thrown away.
        let bare = LOG_LINE.replace(r#""creditUsagePercent":42.5"#, r#""creditUsagePercent":null"#);
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
    fn the_tab_carries_its_subscription_at_every_width_the_wall_uses() {
        // The bar's width is what is left after the labels, and this pane
        // is dragged narrow on a phone. A panicking tab takes the whole
        // widget with it, and a dropped SUBSCRIPTION leaves a percentage
        // that is a percentage of nothing stated.
        let p = palette();
        let d = Data {
            ok: true,
            sessions: 2,
            files: 3,
            total: 12_345.0,
            daily: HashMap::from([(NaiveDate::from_ymd_opt(2026, 8, 16).unwrap(), 900.0)]),
            last: now(),
            quota: newest_quota([LOG_LINE].into_iter()),
        };
        for w in [40usize, 80, 200] {
            let rows = tab(&d, w, 24, &Config::default(), &p);
            assert!(rows.iter().any(|r| r.contains("WEEKLY QUOTA")), "width {}", w);
            assert!(rows.iter().any(|r| r.contains("SUBSCRIPTION")), "width {}", w);
        }
    }

    #[test]
    fn a_period_the_server_did_not_name_is_still_labelled() {
        assert_eq!(period_name("USAGE_PERIOD_TYPE_WEEKLY"), "weekly");
        assert_eq!(period_name("USAGE_PERIOD_TYPE_MONTHLY"), "monthly");
        assert_eq!(period_name(""), "current");
    }

    fn day_of(ms: i64) -> String {
        Utc.timestamp_millis_opt(ms)
            .single()
            .expect("a fixed timestamp")
            .date_naive()
            .to_string()
    }
}
