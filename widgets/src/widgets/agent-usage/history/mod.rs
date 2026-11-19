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

//! Utilisation history: how much of each periodic allowance was used, kept
//! for as long as the machine keeps the file (OPS-303, spec in the Notion
//! page "agent-usage: utilisation history spec").
//!
//! Passive: it is a by-product of the readings the widget already fetches.
//! After every pass the poller turns the pass into [`Observation`]s
//! (`vendors::observations`) and hands them to [`Recorder::pass`], which
//! writes what changed to `agent-usage-history.sqlite` and folds it.
//!
//! * [`store`] - the file: open, migrate, schema, writes, re-folds.
//! * [`fold`] - the pure cycle fold, coverage, and where resets plug in.
//! * [`resets`] - reset detection and bank events (phase 2, empty now).
//! * [`overage`] - spend past an allowance (phase 4, empty now).
//! * [`backfill`] - the one-off Codex rollout replay, read off the poller.
//!
//! Every timestamp is wall-clock epoch seconds of a *fetch*, never of the
//! pass, so a pane that slept, or a reading another pane fetched, lands at
//! the time it was actually read (§4.1).

// The seams are wired and called, but their bodies are later phases' and
// return nothing yet, so the types they hand back are not built by this one.
// Each phase removes its module's allowance when it fills the body.
pub mod backfill;
// The fold's types carry what phase 2 (resets) and phase 3 (charts) read.
#[allow(dead_code)]
pub mod fold;
#[allow(dead_code)]
pub mod overage;
#[allow(dead_code)]
pub mod resets;
// Read helpers (`cycles`, `samples`) are phase 3's, for the charts.
#[allow(dead_code)]
pub mod store;

use std::collections::HashMap;

use crate::shared::{Lane, Spacexsi};
#[cfg(test)]
use fold::Source;
pub use fold::{Sample, Thresholds};

/// At most one sample per pool in this many seconds (§3.7).
pub const MIN_GAP: f64 = 300.0;
/// A sample is written after this long even when nothing changed: proof
/// opscope was watching during a flat stretch (§3.7).
pub const HEARTBEAT: f64 = 3600.0;
/// How long a file that could not be opened is left before trying again.
pub const RETRY_OPEN: f64 = 60.0;

/// One credit in a reset bank, as the server listed it - spent and expired
/// ones included. The history diffs banks by credit (§5.6), so it keeps the
/// raw list rather than the count the pane draws.
#[derive(Clone, Debug, PartialEq)]
pub struct RawCredit {
    /// The server's id for the credit, where it sends one.
    pub id: Option<String>,
    /// `available`, `spent`, or whatever the server said.
    pub status: String,
    pub expires_at: Option<f64>,
}

/// A bank reading: a row of `bank_sample`.
#[derive(Clone, Debug, PartialEq)]
pub struct BankReading {
    /// The bank's own fetch time, which can differ from the lanes'.
    pub at: f64,
    /// The count the server stated, or the credits it listed as available -
    /// not filtered by this machine's clock.
    pub left: u64,
    /// `None` when the server sent a count and no list.
    pub credits: Option<Vec<RawCredit>>,
}

impl BankReading {
    /// The credit list as stored: JSON `[{id?, status, expires_at}]`.
    pub fn credits_json(&self) -> Option<String> {
        let list = self.credits.as_ref()?;
        let rows: Vec<serde_json::Value> = list
            .iter()
            .map(|c| {
                let mut o = serde_json::json!({"status": c.status, "expires_at": c.expires_at});
                if let Some(id) = &c.id {
                    o["id"] = serde_json::json!(id);
                }
                o
            })
            .collect();
        Some(serde_json::Value::Array(rows).to_string())
    }

    /// Rebuilt from a stored row.
    pub fn from_row(at: f64, left: u64, credits: Option<&str>) -> BankReading {
        let credits = credits
            .and_then(|c| serde_json::from_str::<serde_json::Value>(c).ok())
            .and_then(|v| {
                v.as_array().map(|list| {
                    list.iter()
                        .map(|c| RawCredit {
                            id: c["id"].as_str().map(str::to_string),
                            status: c["status"].as_str().unwrap_or_default().to_string(),
                            expires_at: c["expires_at"].as_f64(),
                        })
                        .collect()
                })
            });
        BankReading { at, left, credits }
    }
}

/// The account's plan, as one reading named it: a row of `plan_sample`.
#[derive(Clone, Debug, PartialEq)]
pub struct PlanReading {
    pub at: f64,
    pub plan: String,
}

/// Why readings stopped, where the pane knows (§5.2): a row of
/// `watch_state`.
#[derive(Clone, Debug, PartialEq)]
pub struct WatchReading {
    pub at: f64,
    /// `ok`, `auth`, `logged_out`, `network`, `refused` or `error`.
    pub state: String,
    /// The pane's own wording, which never holds a secret; empty for `ok`.
    pub reason: String,
}

/// What a provider knows about its account beyond its lanes, for the
/// history: each provider's `history_facts` returns one.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Facts {
    /// The provider's stable account id, or empty when it exposes none.
    pub account_id: String,
    pub plan: Option<PlanReading>,
    pub bank: Option<BankReading>,
    /// Why there is no live reading, in the pane's own words; empty while
    /// it is answering.
    pub why: String,
}

/// Everything one pass knows about one account of one provider.
#[derive(Clone, Debug, Default)]
pub struct Observation {
    /// The family id: `claude`, `codex`, `cursor`, ...
    pub provider: String,
    /// The provider's stable id for the account (Claude's account uuid,
    /// Codex's `account_id`, ...). Empty when the provider exposes none.
    pub account_id: String,
    /// What keys the account when there is no stable id: the configured
    /// profile's label, or empty for the default.
    pub profile: String,
    /// The `[+]` heading, display only.
    pub heading: String,
    /// The tab this observation is drawn on, so the tab can find its line.
    pub tab: String,
    pub lanes: Vec<Lane>,
    /// Where a Cursor or Grok account stands relative to the unified plan.
    pub spacexsi: Option<Spacexsi>,
    /// When the account is on the unified plan: the account key the
    /// SpaceXSI copy of its lanes is written under (§3.4). Both vendors of
    /// one shared account give the same key.
    pub spacexsi_account: Option<String>,
    pub bank: Option<BankReading>,
    pub plan: Option<PlanReading>,
    pub watch: Option<WatchReading>,
}

impl Observation {
    /// The key the account is stored under: FNV-1a of the stable id, or the
    /// profile when there is none. Never a credential, which rotates.
    pub fn account_key(&self) -> String {
        if !self.account_id.is_empty() {
            return tc_account_tag(&self.account_id);
        }
        let profile = if self.profile.is_empty() {
            "default"
        } else {
            self.profile.as_str()
        };
        format!("profile:{profile}")
    }

    /// Whether the key came from the provider's own id.
    pub fn has_stable_id(&self) -> bool {
        !self.account_id.is_empty()
    }
}

fn tc_account_tag(id: &str) -> String {
    opscope_core::readings::account_tag(id)
}

/// The SQLite name for a Spacexsi state.
pub fn spacexsi_name(s: Spacexsi) -> &'static str {
    match s {
        Spacexsi::No => "no",
        Spacexsi::Alone => "alone",
        Spacexsi::Shared => "shared",
        Spacexsi::Separate => "separate",
    }
}

fn opt_differs(a: Option<f64>, b: Option<f64>, by: f64) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => (a - b).abs() > by,
        (None, None) => false,
        _ => true,
    }
}

/// Whether two readings of one pool say something different (§3.7): the
/// figure, an amount, the window, or the reset instant beyond
/// [`fold::tolerance`] - a reset read as "now plus seconds left" jitters by
/// the request's latency, and that is not a change.
pub fn changed(a: &Sample, b: &Sample) -> bool {
    opt_differs(a.pct, b.pct, 1e-6)
        || opt_differs(a.used, b.used, 1e-9)
        || opt_differs(a.limit, b.limit, 1e-9)
        || opt_differs(a.window_secs, b.window_secs, 1.0)
        || opt_differs(a.reset_at, b.reset_at, fold::tolerance(b.window_secs.or(a.window_secs)))
}

/// Whether `new` is written, given the stored samples either side of it
/// (§3.7): never within [`MIN_GAP`] of another; otherwise on a change from
/// the one before, or when [`HEARTBEAT`] has passed since it.
pub fn should_write(before: Option<&Sample>, after: Option<&Sample>, new: &Sample) -> bool {
    let near = |s: Option<&Sample>| s.is_some_and(|s| (s.at - new.at).abs() < MIN_GAP);
    if near(before) || near(after) {
        return false;
    }
    match before {
        None => true,
        Some(p) => changed(p, new) || new.at - p.at >= HEARTBEAT,
    }
}

/// What the tab draws about the history.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    /// tab -> the first sample time of the account it draws. `spacexsi`
    /// for the unified plan's own history.
    pub since: HashMap<String, f64>,
    /// Why the history is not being written, or what happened to it - a
    /// newer file left alone, a damaged one set aside, a write that failed.
    pub note: Option<String>,
    /// tab -> a few dim words about where its history came from: the Codex
    /// backfill running, what it wrote, or why it stopped.
    pub notes: HashMap<String, String>,
}

/// `history since 3 Sep 2026`, in the local calendar.
pub fn since_text(at: f64) -> String {
    use chrono::{Local, TimeZone};
    match Local.timestamp_opt(at.trunc() as i64, 0).single() {
        Some(t) => format!("history since {}", t.format("%-d %b %Y")),
        None => "history since ?".into(),
    }
}

/// Record one pass (spec §4): every lane, bank, plan and watch state that
/// changed, under the §3.7 rules, then fold what moved - all in one
/// `BEGIN IMMEDIATE` transaction that reads the previous state from the
/// file. The seams it calls on the way are [`resets::detect`] and
/// [`resets::bank_events`] (phase 2), [`overage::spans`],
/// [`overage::events_of`] and [`overage::links_of`] (phase 4). The Codex
/// backfill is not part of a pass: [`Recorder`] runs it beside them.
///
/// An error naming a busy file ([`store::busy`]) is another pane holding
/// it, and passes.
pub fn observe(h: &mut store::History, obs: &[Observation], t: &Thresholds, now: f64) -> Result<(), String> {
    h.record(obs, t, now)
}

/// The poller's handle on the history file: opens it, keeps it open, says
/// why when it cannot, and records one pass at a time.
pub struct Recorder {
    path: std::path::PathBuf,
    history: Option<store::History>,
    tried_at: f64,
    /// Why the file is not open, or what opening it did (a damaged file
    /// set aside): kept for this pane's life, so it is said, and said once.
    note: Option<String>,
    /// A write that failed on the last pass, other than a busy file.
    failed: Option<String>,
    /// The one-off Codex backfill, read on a thread of its own (§8).
    backfill: backfill::Runner,
}

impl Recorder {
    pub fn new(path: std::path::PathBuf) -> Recorder {
        Recorder::with_rollouts(path, backfill::rollout_roots())
    }

    /// A recorder that backfills Codex from the rollouts under `roots`.
    pub fn with_rollouts(path: std::path::PathBuf, roots: Vec<std::path::PathBuf>) -> Recorder {
        Recorder {
            path,
            history: None,
            tried_at: f64::NEG_INFINITY,
            note: None,
            failed: None,
            backfill: backfill::Runner::new(roots),
        }
    }

    /// Record one pass and say what the tab should draw.
    pub fn pass(&mut self, obs: &[Observation], t: &Thresholds, now: f64) -> Status {
        if self.history.is_none() && now - self.tried_at >= RETRY_OPEN {
            self.tried_at = now;
            match store::History::open(&self.path) {
                Ok(h) => {
                    // A damaged file set aside is said once, and kept said:
                    // a note that vanished on the next pass would be missed.
                    self.note = h.notice.clone();
                    self.history = Some(h);
                }
                Err(store::OpenError::Newer(v)) => {
                    self.note = Some(format!(
                        "history paused: {} is from a newer opscope (schema {v}, this is {}) and is left alone",
                        store::FILE,
                        store::SCHEMA_VERSION
                    ));
                }
                Err(store::OpenError::Failed(why)) => {
                    self.note = Some(format!("history not recorded: {why}"));
                }
            }
        }
        let mut status = Status::default();
        if let Some(h) = self.history.as_mut() {
            match observe(h, obs, t, now) {
                Ok(()) => self.failed = None,
                // Another pane holds the file; this pass's readings are
                // recorded by the next pass that changes or heartbeats.
                Err(why) if store::busy(&why) => {}
                Err(why) => self.failed = Some(format!("history not recorded: {why}")),
            }
            if let Some((tab, words)) = self.backfill.tick(h, obs, t, now) {
                status.notes.insert(tab, words);
            }
            for o in obs {
                // A unified account's tab draws the plan's own history.
                let at = match &o.spacexsi_account {
                    Some(key) => h.since("spacexsi", key),
                    None => h.since(&o.provider, &o.account_key()),
                };
                if let Some(at) = at {
                    let e = status.since.entry(o.tab.clone()).or_insert(at);
                    *e = e.min(at);
                }
            }
        }
        status.note = self.failed.clone().or_else(|| self.note.clone());
        status
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(at: f64, pct: f64, reset: Option<f64>) -> Sample {
        Sample {
            at,
            pct: Some(pct),
            used: None,
            limit: None,
            window_secs: Some(18000.0),
            reset_at: reset,
            source: Source::Live,
            spacexsi: None,
        }
    }

    #[test]
    fn a_sample_is_written_on_a_change_or_a_heartbeat_and_never_twice_in_five_minutes() {
        let p = s(1000.0, 10.0, Some(20000.0));
        // First ever.
        assert!(should_write(None, None, &p));
        // The same held reading again: same fetch time.
        assert!(!should_write(Some(&p), None, &s(1000.0, 10.0, Some(20000.0))));
        // Changed, but inside five minutes.
        assert!(!should_write(Some(&p), None, &s(1200.0, 12.0, Some(20000.0))));
        // Changed, past five minutes.
        assert!(should_write(Some(&p), None, &s(1400.0, 12.0, Some(20000.0))));
        // Unchanged, under an hour.
        assert!(!should_write(Some(&p), None, &s(1000.0 + 3000.0, 10.0, Some(20000.0))));
        // Unchanged, an hour on: the heartbeat.
        assert!(should_write(Some(&p), None, &s(1000.0 + 3600.0, 10.0, Some(20000.0))));
        // A reset instant that jitters by seconds is no change.
        assert!(!should_write(Some(&p), None, &s(1400.0, 10.0, Some(20030.0))));
        // One that moved by more than the tolerance is.
        assert!(should_write(Some(&p), None, &s(1400.0, 10.0, Some(20400.0))));
    }

    #[test]
    fn an_out_of_order_sample_is_written_only_clear_of_both_neighbours() {
        let a = s(1000.0, 10.0, None);
        let b = s(5000.0, 30.0, None);
        assert!(should_write(Some(&a), Some(&b), &s(3000.0, 20.0, None)));
        assert!(!should_write(Some(&a), Some(&b), &s(4900.0, 20.0, None)));
    }

    #[test]
    fn the_account_key_is_the_stable_id_hashed_or_the_profile() {
        let mut o = Observation {
            provider: "claude".into(),
            account_id: "uuid-1".into(),
            ..Observation::default()
        };
        let key = o.account_key();
        assert_eq!(key.len(), 16);
        assert!(o.has_stable_id());
        // Two config dirs on the same account share one key.
        let other = Observation {
            provider: "claude".into(),
            account_id: "uuid-1".into(),
            profile: "work".into(),
            ..Observation::default()
        };
        assert_eq!(other.account_key(), key);
        o.account_id.clear();
        assert_eq!(o.account_key(), "profile:default");
        o.profile = "work".into();
        assert_eq!(o.account_key(), "profile:work");
    }

    #[test]
    fn a_bank_keeps_its_raw_credit_list_through_a_row() {
        let b = BankReading {
            at: 5.0,
            left: 1,
            credits: Some(vec![
                RawCredit {
                    id: Some("c1".into()),
                    status: "available".into(),
                    expires_at: Some(100.0),
                },
                RawCredit {
                    id: None,
                    status: "spent".into(),
                    expires_at: None,
                },
            ]),
        };
        let json = b.credits_json().unwrap();
        assert_eq!(BankReading::from_row(5.0, 1, Some(&json)), b);
        let count_only = BankReading {
            at: 5.0,
            left: 3,
            credits: None,
        };
        assert_eq!(count_only.credits_json(), None);
        assert_eq!(BankReading::from_row(5.0, 3, None), count_only);
    }

    #[test]
    fn the_since_line_names_a_day() {
        let text = since_text(1_763_510_400.0);
        assert!(text.starts_with("history since "), "{text}");
        assert!(text.contains("2025"), "{text}");
    }
}
